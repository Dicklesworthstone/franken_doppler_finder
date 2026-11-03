//! AWS Terrain Tiles (terrarium PNG encoding), cached on disk, as an [`Elevation`] provider.
//!
//! Elevation (m) = R*256 + G + B/256 - 32768. Sampling matches the reference oracle exactly:
//! web-mercator tile coordinates, pixel centres at +0.5, bilinear within one tile (clamped at
//! the tile edge, no cross-tile blending).

use crate::DataError;
use fdop_types::{BBox, Elevation};
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

pub const TERRARIUM_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";
const TILE: usize = 256;

type TileCell = Arc<OnceLock<Option<Arc<Vec<f32>>>>>;

pub struct TerrariumDem {
    zoom: u32,
    dir: PathBuf,
    tiles: RwLock<HashMap<(u32, u32), TileCell>>,
    fallback_m: f64,
    offline: bool,
    failures: AtomicUsize,
    fetched: AtomicUsize,
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
    /// Tiles that could not be loaded (each counted once).
    pub fn failures(&self) -> usize {
        self.failures.load(Ordering::Relaxed)
    }
    /// Tiles downloaded by this instance.
    pub fn fetched(&self) -> usize {
        self.fetched.load(Ordering::Relaxed)
    }

    pub fn tile_path(&self, x: u32, y: u32) -> PathBuf {
        self.dir
            .join(self.zoom.to_string())
            .join(x.to_string())
            .join(format!("{y}.png"))
    }

    fn cell(&self, x: u32, y: u32) -> TileCell {
        if let Some(c) = self.tiles.read().ok().and_then(|m| m.get(&(x, y)).cloned()) {
            return c;
        }
        let mut m = self.tiles.write().unwrap_or_else(|p| p.into_inner());
        m.entry((x, y)).or_default().clone()
    }

    fn load_tile(&self, x: u32, y: u32) -> Option<Arc<Vec<f32>>> {
        let path = self.tile_path(x, y);
        if let Ok(b) = std::fs::read(&path)
            && let Ok(g) = decode_terrarium_png(&b)
        {
            return Some(Arc::new(g));
        }
        if self.offline {
            self.failures.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let url = format!("{TERRARIUM_URL}/{}/{x}/{y}.png", self.zoom);
        let got = with_net(|net| net.get(&url, &[])).ok().and_then(|r| r.ok());
        let Some(resp) = got else {
            self.failures.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        match decode_terrarium_png(&resp.body) {
            Ok(g) => {
                self.fetched.fetch_add(1, Ordering::Relaxed);
                if let Some(d) = path.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let tmp = path.with_extension("png.part");
                if std::fs::write(&tmp, &resp.body).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                Some(Arc::new(g))
            }
            Err(_) => {
                self.failures.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn tile(&self, x: u32, y: u32) -> Option<Arc<Vec<f32>>> {
        self.cell(x, y).get_or_init(|| self.load_tile(x, y)).clone()
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

impl Elevation for TerrariumDem {
    fn elev_m(&self, lat: f64, lon: f64) -> f64 {
        let (xf, yf) = tile_xy(lat, lon, self.zoom);
        let (tx, ty) = (xf.floor(), yf.floor());
        let Some(tile) = self.tile(tx as u32, ty as u32) else {
            return self.fallback_m;
        };
        let px = ((xf - tx) * 256.0 - 0.5).clamp(0.0, 254.999);
        let py = ((yf - ty) * 256.0 - 0.5).clamp(0.0, 254.999);
        let (x0, y0) = (px.floor() as usize, py.floor() as usize);
        let (wx, wy) = (px - x0 as f64, py - y0 as f64);
        let g = |y: usize, x: usize| f64::from(tile[y * TILE + x]);
        g(y0, x0) * (1.0 - wx) * (1.0 - wy)
            + g(y0, x0 + 1) * wx * (1.0 - wy)
            + g(y0 + 1, x0) * (1.0 - wx) * wy
            + g(y0 + 1, x0 + 1) * wx * wy
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
}
