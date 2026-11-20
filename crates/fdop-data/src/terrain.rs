//! AWS Terrain Tiles (terrarium PNG encoding), cached on disk, as an [`Elevation`] provider.
//!
//! Elevation (m) = R*256 + G + B/256 - 32768. Sampling matches the reference oracle exactly:
//! web-mercator tile coordinates, pixel centres at +0.5, bilinear within one tile (clamped at
//! the tile edge, no cross-tile blending). If the requested tile is unavailable, an already
//! cached parent down to z10 (the pipeline's coarse terrain resolution) can supply elevation.
//! Recovery never downloads a parent, and its lower resolution is counted for reporting.
//!
//! One exception: a pixel below [`LOWEST_GROUND_M`] is left out of the blend. The elevation
//! here is only ever the observer's ground, and no ground lies that low, so such a pixel is
//! seabed or a spike in the tiles. The cached z13 tiles near the development sites have
//! single pixels of -796 to -1026 m beside pixels of 0 to 3 m, and z12 one of -1972 m. Those
//! put candidates hundreds of metres under the sea, outside the sound-speed column.

use crate::DataError;
use fdop_types::{BBox, Elevation};
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

pub const TERRARIUM_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";
const TILE: usize = 256;
/// Do not replace missing fine terrain by a surface coarser than the location search uses.
const MIN_RECOVERY_ZOOM: u32 = 10;

/// The lowest dry land on Earth, the Dead Sea shore (about -430 m). A tile value below it is
/// not a surface anyone records from.
pub const LOWEST_GROUND_M: f32 = -430.0;

#[derive(Clone)]
struct TerrainTile {
    zoom: u32,
    heights: Arc<Vec<f32>>,
}

type TileCell = Arc<OnceLock<Option<TerrainTile>>>;

pub struct TerrariumDem {
    zoom: u32,
    dir: PathBuf,
    tiles: RwLock<HashMap<(u32, u32, u32), TileCell>>,
    fallback_m: f64,
    offline: bool,
    failures: AtomicUsize,
    fetched: AtomicUsize,
    recovered: AtomicUsize,
    lowest_zoom: AtomicU32,
}

thread_local! {
    static NET: RefCell<Option<fdop_http::Net>> = const { RefCell::new(None) };
}

fn with_net<T>(f: impl FnOnce(&fdop_http::Net) -> T) -> Result<T, DataError> {
    NET.with(|cell| {
        let mut b = cell.borrow_mut();
        if b.is_none() {
            *b = Some(fdop_http::Net::new()?);
        }
        Ok(f(b.as_ref().expect("net initialised")))
    })
}

/// Decode a terrarium PNG into a row-major 256x256 elevation grid.
pub fn decode_terrarium_png(bytes: &[u8]) -> Result<Vec<f32>, DataError> {
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| DataError(format!("png: {e}")))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| DataError(format!("png frame: {e}")))?;
    let ch = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => {
            return Err(DataError(format!(
                "terrarium tile has colour type {other:?}, want RGB/RGBA"
            )));
        }
    };
    let (w, h) = (info.width as usize, info.height as usize);
    if w != TILE || h != TILE {
        return Err(DataError(format!(
            "terrarium tile is {w}x{h}, want 256x256"
        )));
    }
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = &buf[y * info.line_size..y * info.line_size + w * ch];
        for x in 0..w {
            let p = &row[x * ch..x * ch + 3];
            out.push(
                (f32::from(p[0]) * 256.0 + f32::from(p[1]) + f32::from(p[2]) / 256.0) - 32768.0,
            );
        }
    }
    Ok(out)
}

/// Fractional web-mercator tile coordinates of (lat, lon) at zoom `z`.
pub fn tile_xy(lat: f64, lon: f64, z: u32) -> (f64, f64) {
    let n = f64::from(1u32 << z);
    let la = lat.clamp(-85.051_128, 85.051_128).to_radians();
    let xf = (lon + 180.0) / 360.0 * n;
    let yf = (1.0 - (la.tan() + 1.0 / la.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (xf, yf)
}

impl TerrariumDem {
    /// Tiles cached under `<cache_dir>/terrarium/{z}/{x}/{y}.png`.
    pub fn new(cache_dir: &Path, zoom: u32) -> Self {
        Self {
            zoom,
            dir: cache_dir.join("terrarium"),
            tiles: RwLock::new(HashMap::new()),
            fallback_m: 0.0,
            offline: false,
            failures: AtomicUsize::new(0),
            fetched: AtomicUsize::new(0),
            recovered: AtomicUsize::new(0),
            lowest_zoom: AtomicU32::new(zoom),
        }
    }
    /// Elevation returned for tiles that cannot be fetched or decoded.
    pub fn with_fallback(mut self, m: f64) -> Self {
        self.fallback_m = m;
        self
    }
    /// Never fetch; use cached tiles only.
    pub fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }
    pub fn zoom(&self) -> u32 {
        self.zoom
    }
    /// Requested tiles with neither their own data nor a usable cached parent (counted once).
    pub fn failures(&self) -> usize {
        self.failures.load(Ordering::Relaxed)
    }
    /// Tiles downloaded by this instance.
    pub fn fetched(&self) -> usize {
        self.fetched.load(Ordering::Relaxed)
    }
    /// Requested tiles supplied by a cached parent (each counted once, even across threads).
    pub fn recovered_tiles(&self) -> usize {
        self.recovered.load(Ordering::Relaxed)
    }
    /// Coarsest resolution used; the requested zoom when no parent recovery was needed.
    pub fn lowest_used_zoom(&self) -> u32 {
        self.lowest_zoom.load(Ordering::Relaxed)
    }

    pub fn tile_path(&self, x: u32, y: u32) -> PathBuf {
        self.path_at(self.zoom, x, y)
    }

    fn path_at(&self, zoom: u32, x: u32, y: u32) -> PathBuf {
        self.dir
            .join(zoom.to_string())
            .join(x.to_string())
            .join(format!("{y}.png"))
    }

    fn cell(&self, zoom: u32, x: u32, y: u32) -> TileCell {
        let key = (zoom, x, y);
        if let Some(c) = self.tiles.read().ok().and_then(|m| m.get(&key).cloned()) {
            return c;
        }
        let mut m = self.tiles.write().unwrap_or_else(|p| p.into_inner());
        m.entry(key).or_default().clone()
    }

    fn cached_tile(&self, zoom: u32, x: u32, y: u32) -> Option<TerrainTile> {
        let bytes = std::fs::read(self.path_at(zoom, x, y)).ok()?;
        let heights = decode_terrarium_png(&bytes).ok()?;
        Some(TerrainTile {
            zoom,
            heights: Arc::new(heights),
        })
    }

    fn load_tile(&self, x: u32, y: u32) -> Option<TerrainTile> {
        let path = self.tile_path(x, y);
        if let Some(tile) = self.cached_tile(self.zoom, x, y) {
            return Some(tile);
        }
        if !self.offline {
            let url = format!("{TERRARIUM_URL}/{}/{x}/{y}.png", self.zoom);
            let got = with_net(|net| net.get(&url, &[])).ok().and_then(|r| r.ok());
            if let Some(resp) = got
                && let Ok(heights) = decode_terrarium_png(&resp.body)
            {
                self.fetched.fetch_add(1, Ordering::Relaxed);
                let _ = fdop_http::write_atomic(&path, &resp.body);
                return Some(TerrainTile {
                    zoom: self.zoom,
                    heights: Arc::new(heights),
                });
            }
        }
        for zoom in (MIN_RECOVERY_ZOOM..self.zoom).rev() {
            let (px, py) = (x >> (self.zoom - zoom), y >> (self.zoom - zoom));
            // Many requested tiles share one parent. Cache its immutable decoded grid,
            // including misses, without opening a network client or duplicating the grid.
            let parent = self.cell(zoom, px, py);
            if let Some(tile) = parent.get_or_init(|| self.cached_tile(zoom, px, py)) {
                self.recovered.fetch_add(1, Ordering::Relaxed);
                self.lowest_zoom.fetch_min(zoom, Ordering::Relaxed);
                return Some(tile.clone());
            }
        }
        self.failures.fetch_add(1, Ordering::Relaxed);
        None
    }

    fn tile(&self, x: u32, y: u32) -> Option<TerrainTile> {
        self.cell(self.zoom, x, y)
            .get_or_init(|| self.load_tile(x, y))
            .clone()
    }

    /// Load (fetching as needed) every tile covering `bbox`, in parallel.
    pub fn prefetch_bbox(&self, bbox: &BBox, max_tiles: usize) -> Result<usize, DataError> {
        let (x0, y0) = tile_xy(bbox.north, bbox.west, self.zoom);
        let (x1, y1) = tile_xy(bbox.south, bbox.east, self.zoom);
        let (xa, xb) = (x0.floor() as u32, x1.floor() as u32);
        let (ya, yb) = (y0.floor() as u32, y1.floor() as u32);
        let n = (xb - xa + 1) as usize * (yb - ya + 1) as usize;
        if n > max_tiles {
            return Err(DataError(format!(
                "terrain prefetch needs {n} tiles at z{} (> max {max_tiles}); lower the zoom or shrink the bbox",
                self.zoom
            )));
        }
        let coords: Vec<(u32, u32)> = (xa..=xb)
            .flat_map(|x| (ya..=yb).map(move |y| (x, y)))
            .collect();
        let ok = coords
            .par_iter()
            .with_max_len(1)
            .filter(|(x, y)| self.tile(*x, *y).is_some())
            .count();
        Ok(ok)
    }
}

/// Bilinear blend of a cell's corners, leaving out any below [`LOWEST_GROUND_M`] and
/// renormalising the rest. With every corner left out the cell is open water, whose surface is
/// sea level. A kept corner whose weight is zero (the point sits on the excluded one) still
/// counts, as the plain mean of the kept corners.
fn blend_ground(corners: &[(f32, f64); 4]) -> f64 {
    let kept = corners.iter().filter(|(v, _)| *v >= LOWEST_GROUND_M);
    let (sum, weight, n, plain) = kept.fold((0.0, 0.0, 0usize, 0.0), |(s, w, n, p), (v, k)| {
        let v = f64::from(*v);
        (s + v * k, w + k, n + 1, p + v)
    });
    if n == 0 {
        0.0
    } else if weight > 1e-9 {
        sum / weight
    } else {
        plain / n as f64
    }
}

impl Elevation for TerrariumDem {
    fn elev_m(&self, lat: f64, lon: f64) -> f64 {
        let (xf, yf) = tile_xy(lat, lon, self.zoom);
        let (tx, ty) = (xf.floor(), yf.floor());
        let Some(tile) = self.tile(tx as u32, ty as u32) else {
            return self.fallback_m;
        };
        // Sample the observer's coordinates in the selected parent's grid, not the
        // original fine tile's fractional coordinates (which repeat in every child).
        let scale = f64::from(1u32 << (self.zoom - tile.zoom));
        let (xf, yf) = (xf / scale, yf / scale);
        let (tx, ty) = (xf.floor(), yf.floor());
        let tile = &tile.heights;
        let px = ((xf - tx) * 256.0 - 0.5).clamp(0.0, 254.999);
        let py = ((yf - ty) * 256.0 - 0.5).clamp(0.0, 254.999);
        let (x0, y0) = (px.floor() as usize, py.floor() as usize);
        let (wx, wy) = (px - x0 as f64, py - y0 as f64);
        let corners = [
            (tile[y0 * TILE + x0], (1.0 - wx) * (1.0 - wy)),
            (tile[y0 * TILE + x0 + 1], wx * (1.0 - wy)),
            (tile[(y0 + 1) * TILE + x0], (1.0 - wx) * wy),
            (tile[(y0 + 1) * TILE + x0 + 1], wx * wy),
        ];
        blend_ground(&corners)
    }

    fn describe(&self) -> String {
        format!("terrarium:z{}", self.zoom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(elev: impl Fn(usize, usize) -> f64) -> Vec<u8> {
        let mut data = Vec::with_capacity(TILE * TILE * 3);
        for y in 0..TILE {
            for x in 0..TILE {
                let v = elev(x, y) + 32768.0;
                let r = (v / 256.0).floor();
                let g = (v - r * 256.0).floor();
                let b = ((v - r * 256.0 - g) * 256.0).round();
                data.extend_from_slice(&[r as u8, g as u8, b as u8]);
            }
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, TILE as u32, TILE as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn decode_and_bilinear_match_oracle() {
        let elev = |x: usize, y: usize| 10.0 * x as f64 + 0.5 * y as f64 - 20.0;
        let png_bytes = encode(elev);
        let g = decode_terrarium_png(&png_bytes).unwrap();
        assert!((f64::from(g[3 * TILE + 7]) - elev(7, 3)).abs() < 0.01);
        let dir = std::env::temp_dir().join(format!("fdop-terrain-test-{}", std::process::id()));
        let z = 12;
        let (lat, lon) = (40.876466, -73.524951);
        let (xf, yf) = tile_xy(lat, lon, z);
        let dem = TerrariumDem::new(&dir, z)
            .with_offline(true)
            .with_fallback(-999.0);
        let p = dem.tile_path(xf as u32, yf as u32);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, &png_bytes).unwrap();
        let px = (xf - xf.floor()) * 256.0 - 0.5;
        let py = (yf - yf.floor()) * 256.0 - 0.5;
        // the field is linear, so bilinear reproduces it exactly
        let want = 10.0 * px + 0.5 * py - 20.0;
        assert!(
            (dem.elev_m(lat, lon) - want).abs() < 0.01,
            "{} vs {want}",
            dem.elev_m(lat, lon)
        );
        // a missing tile offline -> fallback, counted once
        assert_eq!(dem.elev_m(10.0, 10.0), -999.0);
        assert_eq!(dem.elev_m(10.0, 10.0), -999.0);
        assert_eq!(dem.failures(), 1);
        assert_eq!(dem.describe(), "terrarium:z12");
    }

    const AT: (f64, f64) = (40.876466, -73.524951);
    const Z: u32 = 13;

    fn fresh_cache(tag: &str) -> PathBuf {
        let mut suffix = 0u64;
        loop {
            let path = std::env::temp_dir().join(format!(
                "fdop-terrain-{tag}-{}-{suffix}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
                Err(error) => panic!("create {}: {error}", path.display()),
            }
        }
    }

    fn put_tile(dem: &TerrariumDem, zoom: u32, x: u32, y: u32, bytes: &[u8]) {
        let path = dem.path_at(zoom, x, y);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn tile_latlon(x: f64, y: f64, zoom: u32) -> (f64, f64) {
        let n = f64::from(1u32 << zoom);
        (
            (std::f64::consts::PI * (1.0 - 2.0 * y / n))
                .sinh()
                .atan()
                .to_degrees(),
            x / n * 360.0 - 180.0,
        )
    }

    #[test]
    fn cached_parents_preserve_geographic_pixels_across_all_children() {
        let surface = |x: f64, y: f64| 600.0 + 2.0 * x + 3.0 * y;
        for zoom in MIN_RECOVERY_ZOOM..Z {
            let dir = fresh_cache(&format!("parent-z{zoom}"));
            let dem = TerrariumDem::new(&dir, Z)
                .with_offline(true)
                .with_fallback(-999.0);
            let (x, y) = tile_xy(AT.0, AT.1, zoom);
            let (x, y) = (x.floor() as u32, y.floor() as u32);
            put_tile(
                &dem,
                zoom,
                x,
                y,
                &encode(|x, y| surface(x as f64, y as f64)),
            );
            let scale = 1u32 << (Z - zoom);
            let mut samples = Vec::new();
            for cx in 0..scale {
                for cy in 0..scale {
                    // Both sides of every child boundary plus its interior; outer
                    // parent edges must retain the existing half-pixel clamp.
                    for dx in [0.000_01, 0.5, 0.999_99] {
                        for dy in [0.000_01, 0.5, 0.999_99] {
                            let fx = f64::from(x * scale + cx) + dx;
                            let fy = f64::from(y * scale + cy) + dy;
                            let (lat, lon) = tile_latlon(fx, fy, Z);
                            let px = ((f64::from(cx) + dx) / f64::from(scale) * 256.0 - 0.5)
                                .clamp(0.0, 254.999);
                            let py = ((f64::from(cy) + dy) / f64::from(scale) * 256.0 - 0.5)
                                .clamp(0.0, 254.999);
                            samples.push((lat, lon, surface(px, py)));
                        }
                    }
                }
            }
            samples.par_iter().for_each(|&(lat, lon, want)| {
                let got = dem.elev_m(lat, lon);
                assert!((got - want).abs() < 1e-6, "z{zoom}: {got} vs {want}");
            });
            assert_eq!(dem.recovered_tiles(), (scale * scale) as usize);
            assert_eq!(dem.lowest_used_zoom(), zoom);
            assert_eq!(dem.failures(), 0);
            assert_eq!(dem.fetched(), 0);
            let first = dem.tile(x * scale, y * scale).unwrap();
            let last = dem.tile((x + 1) * scale - 1, (y + 1) * scale - 1).unwrap();
            assert!(Arc::ptr_eq(&first.heights, &last.heights));
            assert!(!dem.tile_path(x * scale, y * scale).exists());
        }
    }

    #[test]
    fn terrain_recovery_prefers_finer_data_and_stops_at_the_coarse_search_zoom() {
        // A missing/corrupt child must not hide usable cached terrain, but z9
        // cannot silently replace the z10 minimum used by the coarse search.
        for (tag, requested, good, bad, expected_zoom) in [
            ("exact", 13, vec![10, 11, 12, 13], vec![], Some(13)),
            ("bad-exact", 13, vec![10, 11, 12], vec![13], Some(12)),
            ("bad-parent", 13, vec![10, 11], vec![12], Some(11)),
            ("coarse", 13, vec![10], vec![11, 12, 13], Some(10)),
            ("below-floor", 13, vec![9], vec![10], None),
            ("coarse-missing", 10, vec![9], vec![], None),
        ] {
            let dir = fresh_cache(&format!("recovery-{tag}"));
            let dem = TerrariumDem::new(&dir, requested)
                .with_offline(true)
                .with_fallback(-999.0);
            for &zoom in &good {
                let (x, y) = tile_xy(AT.0, AT.1, zoom);
                put_tile(
                    &dem,
                    zoom,
                    x as u32,
                    y as u32,
                    &encode(|_, _| f64::from(zoom) * 100.0),
                );
            }
            for &zoom in &bad {
                let (x, y) = tile_xy(AT.0, AT.1, zoom);
                put_tile(&dem, zoom, x as u32, y as u32, b"corrupt terrain tile");
            }
            (0..64).into_par_iter().for_each(|_| {
                assert_eq!(
                    dem.elev_m(AT.0, AT.1),
                    expected_zoom.map_or(-999.0, |z| f64::from(z) * 100.0),
                    "{tag}"
                );
            });
            assert_eq!(
                dem.failures(),
                usize::from(expected_zoom.is_none()),
                "{tag}"
            );
            assert_eq!(
                dem.recovered_tiles(),
                usize::from(expected_zoom.is_some_and(|z| z < requested)),
                "{tag}"
            );
            assert_eq!(
                dem.lowest_used_zoom(),
                expected_zoom.unwrap_or(requested),
                "{tag}"
            );
            assert_eq!(dem.fetched(), 0);
            // Each instance's resolution remains immutable during a search. A new
            // instance can use a fine tile cached after this one's first lookup.
            let (x, y) = tile_xy(AT.0, AT.1, requested);
            put_tile(&dem, requested, x as u32, y as u32, &encode(|_, _| 777.0));
            assert_eq!(
                dem.elev_m(AT.0, AT.1),
                expected_zoom.map_or(-999.0, |z| f64::from(z) * 100.0)
            );
            let fresh = TerrariumDem::new(&dir, requested).with_offline(true);
            assert_eq!(fresh.elev_m(AT.0, AT.1), 777.0);
            assert_eq!(fresh.recovered_tiles(), 0);
        }
    }

    /// The reference point's pixel coordinates in its z13 tile.
    fn pixel() -> (f64, f64) {
        let (xf, yf) = tile_xy(AT.0, AT.1, Z);
        (
            (xf - xf.floor()) * 256.0 - 0.5,
            (yf - yf.floor()) * 256.0 - 0.5,
        )
    }

    /// A DEM whose only tile, the one holding the reference point, is `elev`.
    fn one_tile(tag: &str, elev: impl Fn(usize, usize) -> f64) -> TerrariumDem {
        let dir = std::env::temp_dir().join(format!("fdop-terrain-{tag}-{}", std::process::id()));
        let (xf, yf) = tile_xy(AT.0, AT.1, Z);
        let dem = TerrariumDem::new(&dir, Z)
            .with_offline(true)
            .with_fallback(-999.0);
        let p = dem.tile_path(xf as u32, yf as u32);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, encode(elev)).unwrap();
        dem
    }

    #[test]
    fn a_spike_pixel_below_any_ground_is_left_out_of_the_blend() {
        // The cached tiles near the osprey cam have cells like -895.4, 1.0 / -895.4, 1.0.
        let (px, py) = pixel();
        let (sx, sy) = (px.floor() as usize, py.floor() as usize);
        let spike = |x: usize, y: usize| (x, y) == (sx, sy) || (x, y) == (sx, sy + 1);
        let dem = one_tile("spike", |x, y| if spike(x, y) { -895.4 } else { 1.0 });
        let z = dem.elev_m(AT.0, AT.1);
        assert!((z - 1.0).abs() < 1e-6, "{z}");
    }

    #[test]
    fn a_cell_with_no_ground_is_open_water_at_sea_level() {
        let dem = one_tile("ocean", |x, _| -3000.0 - x as f64);
        assert_eq!(dem.elev_m(AT.0, AT.1), 0.0);
    }

    #[test]
    fn real_low_ground_and_values_just_above_the_floor_are_kept() {
        // Death Valley is -86 m; a field that dips to just above the floor is blended as before.
        let (px, py) = pixel();
        let dem = one_tile("low", |x, y| -429.0 + 0.5 * x as f64 + 0.25 * y as f64);
        let want = -429.0 + 0.5 * px + 0.25 * py;
        let z = dem.elev_m(AT.0, AT.1);
        assert!((z - want).abs() < 0.01, "{z} vs {want}");
        let dem = one_tile("badwater", |_, _| -86.0);
        assert!((dem.elev_m(AT.0, AT.1) + 86.0).abs() < 0.01);
    }

    #[test]
    fn a_point_on_an_excluded_corner_takes_the_kept_corners_mean() {
        let w = |a: f32, b: f32, c: f32, d: f32| [(a, 1.0), (b, 0.0), (c, 0.0), (d, 0.0)];
        assert_eq!(blend_ground(&w(-900.0, 2.0, 4.0, -900.0)), 3.0);
        assert_eq!(blend_ground(&w(5.0, -900.0, -900.0, -900.0)), 5.0);
        assert_eq!(blend_ground(&w(-900.0, -900.0, -900.0, -900.0)), 0.0);
        let mixed = [(10.0, 0.25), (-900.0, 0.25), (20.0, 0.25), (-900.0, 0.25)];
        assert_eq!(blend_ground(&mixed), 15.0);
    }
}
