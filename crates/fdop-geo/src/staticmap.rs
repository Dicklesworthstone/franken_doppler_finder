//! Static answer map: USGS Topo basemap tiles (JPEG, Web Mercator) composited into a PNG,
//! plus an SVG twin with the tiles embedded, the uncertainty circle, centre cross, candidate
//! markers, scale bar, north arrow and labels. Offline (or on tile errors) the background is a
//! plain grid with the same overlays.

use crate::font::{GLYPH_H, GLYPH_W, pixel};
use crate::{GeoError, base64, esc};
use std::path::{Path, PathBuf};

pub const USGS_TOPO_URL: &str =
    "https://basemap.nationalmap.gov/arcgis/rest/services/USGSTopo/MapServer/tile";
const TILE: i64 = 256;
const EQUATOR_M_PER_PX_Z0: f64 = 156_543.033_92;

#[derive(Clone, Debug)]
pub struct StaticMapOptions {
    /// fdop cache root; tiles go to `<cache_dir>/usgs_topo/{z}/{y}/{x}.jpg`.
    pub cache_dir: PathBuf,
    /// Square image size in pixels.
    pub size_px: u32,
    /// Title line drawn top-left (e.g. the place headline).
    pub headline: Option<String>,
    /// Extra markers (lat, lon), e.g. runner-up candidates.
    pub markers: Vec<(f64, f64)>,
    /// Highest zoom requested from the tile server.
    pub max_zoom: u8,
    /// Output file stem ("map" -> map.png / map.svg).
    pub stem: String,
}

impl Default for StaticMapOptions {
    fn default() -> Self {
        Self {
            cache_dir: crate::default_cache_dir(),
            size_px: 1024,
            headline: None,
            markers: Vec::new(),
            max_zoom: 16,
            stem: "map".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StaticMap {
    pub png: PathBuf,
    pub svg: PathBuf,
    pub zoom: u8,
    /// Ground metres per pixel at the centre latitude.
    pub m_per_px: f64,
    pub radius_px: f64,
    pub tiles_ok: usize,
    pub tiles_failed: usize,
    /// True when no basemap tile could be drawn.
    pub offline: bool,
}

/// World pixel coordinates (Web Mercator) at zoom `z`.
pub fn world_px(lat: f64, lon: f64, z: u8) -> (f64, f64) {
    let n = (TILE as f64) * f64::from(1u32 << z);
    let la = lat.clamp(-85.051_128, 85.051_128).to_radians();
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - (la.tan() + 1.0 / la.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

/// Zoom at which a circle of `radius_m` spans about a third of `size_px`.
pub fn choose_zoom(lat: f64, radius_m: f64, size_px: u32, max_zoom: u8) -> u8 {
    let target = (2.0 * radius_m.max(1.0)) / (f64::from(size_px) / 3.0);
    let z = (EQUATOR_M_PER_PX_Z0 * lat.to_radians().cos() / target)
        .log2()
        .floor();
    z.clamp(2.0, f64::from(max_zoom)) as u8
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

type Rgb = [u8; 3];

impl Canvas {
    fn new(w: usize, h: usize, bg: Rgb) -> Self {
        let mut px = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            px.extend_from_slice(&bg);
        }
        Self { w, h, px }
    }
    fn blend(&mut self, x: i64, y: i64, c: Rgb, a: f64) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let i = (y as usize * self.w + x as usize) * 3;
        for k in 0..3 {
            let v = f64::from(self.px[i + k]) * (1.0 - a) + f64::from(c[k]) * a;
            self.px[i + k] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    fn rect(&mut self, x0: i64, y0: i64, w: i64, h: i64, c: Rgb, a: f64) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.blend(x, y, c, a);
            }
        }
    }
    fn ring(&mut self, cx: f64, cy: f64, r: f64, half_w: f64, c: Rgb) {
        let ext = r + half_w + 2.0;
        for y in (cy - ext).floor() as i64..=(cy + ext).ceil() as i64 {
            for x in (cx - ext).floor() as i64..=(cx + ext).ceil() as i64 {
                let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
                let a = (half_w + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
                self.blend(x, y, c, a);
            }
        }
    }
    fn disc(&mut self, cx: f64, cy: f64, r: f64, c: Rgb) {
        for y in (cy - r - 1.0).floor() as i64..=(cy + r + 1.0).ceil() as i64 {
            for x in (cx - r - 1.0).floor() as i64..=(cx + r + 1.0).ceil() as i64 {
                let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
                self.blend(x, y, c, (r + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }
    fn triangle(&mut self, p: [(f64, f64); 3], c: Rgb) {
        let (minx, maxx) = (
            p.iter().map(|q| q.0).fold(f64::INFINITY, f64::min),
            p.iter().map(|q| q.0).fold(f64::NEG_INFINITY, f64::max),
        );
        let (miny, maxy) = (
            p.iter().map(|q| q.1).fold(f64::INFINITY, f64::min),
            p.iter().map(|q| q.1).fold(f64::NEG_INFINITY, f64::max),
        );
        let edge = |a: (f64, f64), b: (f64, f64), q: (f64, f64)| {
            (b.0 - a.0) * (q.1 - a.1) - (b.1 - a.1) * (q.0 - a.0)
        };
        for y in miny.floor() as i64..=maxy.ceil() as i64 {
            for x in minx.floor() as i64..=maxx.ceil() as i64 {
                let q = (x as f64 + 0.5, y as f64 + 0.5);
                let (e0, e1, e2) = (
                    edge(p[0], p[1], q),
                    edge(p[1], p[2], q),
                    edge(p[2], p[0], q),
                );
                if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0) {
                    self.blend(x, y, c, 1.0);
                }
            }
        }
    }
    /// Text with a 1-px halo; returns the drawn width.
    fn text(&mut self, x: i64, y: i64, s: &str, scale: i64, c: Rgb, halo: Option<Rgb>) -> i64 {
        let adv = (GLYPH_W as i64 + 1) * scale;
        if let Some(hc) = halo {
            for (i, ch) in s.chars().enumerate() {
                for gy in 0..GLYPH_H {
                    for gx in 0..GLYPH_W {
                        if pixel(ch, gx, gy) {
                            let (bx, by) = (
                                x + i as i64 * adv + gx as i64 * scale,
                                y + gy as i64 * scale,
                            );
                            self.rect(bx - 1, by - 1, scale + 2, scale + 2, hc, 1.0);
                        }
                    }
                }
            }
        }
        for (i, ch) in s.chars().enumerate() {
            for gy in 0..GLYPH_H {
                for gx in 0..GLYPH_W {
                    if pixel(ch, gx, gy) {
                        self.rect(
                            x + i as i64 * adv + gx as i64 * scale,
                            y + gy as i64 * scale,
                            scale,
                            scale,
                            c,
                            1.0,
                        );
                    }
                }
            }
        }
        s.chars().count() as i64 * adv
    }
    fn write_png(&self, path: &Path) -> Result<(), GeoError> {
        let f = std::fs::File::create(path)?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc
            .write_header()
            .map_err(|e| GeoError::Io(format!("png header: {e}")))?;
        w.write_image_data(&self.px)
            .map_err(|e| GeoError::Io(format!("png data: {e}")))?;
        Ok(())
    }
}

/// Fetch (or read from cache) one USGS Topo tile as JPEG bytes.
fn tile_bytes(
    net: Option<&fdop_http::Net>,
    cache_dir: &Path,
    z: u8,
    x: i64,
    y: i64,
) -> Result<Vec<u8>, GeoError> {
    let path = cache_dir
        .join("usgs_topo")
        .join(z.to_string())
        .join(y.to_string())
        .join(format!("{x}.jpg"));
    if let Ok(b) = std::fs::read(&path)
        && b.len() > 100
    {
        return Ok(b);
    }
    let net = net.ok_or_else(|| GeoError::Offline(format!("tile {z}/{y}/{x} not cached")))?;
    net.download_to(&format!("{USGS_TOPO_URL}/{z}/{y}/{x}"), &path)?;
    Ok(std::fs::read(&path)?)
}

fn decode_jpeg(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>), GeoError> {
    let mut dec = jpeg_decoder::Decoder::new(bytes);
    let data = dec
        .decode()
        .map_err(|e| GeoError::Data(format!("jpeg: {e}")))?;
    let info = dec
        .info()
        .ok_or_else(|| GeoError::Data("jpeg: no info".into()))?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => data,
        jpeg_decoder::PixelFormat::L8 => data.iter().flat_map(|&v| [v, v, v]).collect(),
        jpeg_decoder::PixelFormat::L16 => data
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[0], c[0]])
            .collect(),
        jpeg_decoder::PixelFormat::CMYK32 => data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|c| {
                let k = 255 - u16::from(c[3]);
                [c[0], c[1], c[2]].map(|v| ((255 - u16::from(v)) * k / 255) as u8)
            })
            .collect(),
    };
    if rgb.len() < w * h * 3 {
        return Err(GeoError::Data("jpeg: short pixel buffer".into()));
    }
    Ok((w, h, rgb))
}

/// "1 km", "500 m", ...
fn scale_label(m: f64) -> String {
    if m >= 1000.0 {
        format!("{} km", m / 1000.0)
    } else {
        format!("{m} m")
    }
}

fn nice_scale(m_per_px: f64) -> (f64, f64) {
    let mut best = (100.0, 100.0 / m_per_px);
    for k in -1..7 {
        for b in [1.0, 2.0, 5.0] {
            let len = b * 10f64.powi(k);
            let px = len / m_per_px;
            if (100.0..=280.0).contains(&px) {
                return (len, px);
            }
            if (px - 180.0).abs() < (best.1 - 180.0).abs() {
                best = (len, px);
            }
        }
    }
    best
}

/// Render `map.png` + `map.svg` into `out_dir`.
pub fn static_map(
    net: Option<&fdop_http::Net>,
    lat: f64,
    lon: f64,
    radius_m: f64,
    out_dir: &Path,
    opts: &StaticMapOptions,
) -> Result<StaticMap, GeoError> {
    if !(lat.is_finite() && lon.is_finite() && radius_m.is_finite() && radius_m > 0.0) {
        return Err(GeoError::Parse(format!(
            "static_map: bad centre/radius {lat},{lon} r={radius_m}"
        )));
    }
    std::fs::create_dir_all(out_dir)?;
    let n = opts.size_px.max(256) as usize;
    let z = choose_zoom(lat, radius_m, n as u32, opts.max_zoom);
    let m_per_px = EQUATOR_M_PER_PX_Z0 * lat.to_radians().cos() / f64::from(1u32 << z);
    let (cx, cy) = world_px(lat, lon, z);
    let (ox, oy) = (cx - n as f64 / 2.0, cy - n as f64 / 2.0);
    let (ox_i, oy_i) = (ox.floor() as i64, oy.floor() as i64);
    let (ccx, ccy) = (cx - ox_i as f64, cy - oy_i as f64); // centre in image pixels
    let mut cv = Canvas::new(n, n, [232, 236, 239]);
    for k in (0..n as i64).step_by(64) {
        cv.rect(k, 0, 1, n as i64, [205, 212, 219], 1.0);
        cv.rect(0, k, n as i64, 1, [205, 212, 219], 1.0);
    }
    let tiles_per_axis = 1i64 << z;
    let (tx0, tx1) = (
        ox_i.div_euclid(TILE),
        (ox_i + n as i64 - 1).div_euclid(TILE),
    );
    let (ty0, ty1) = (
        oy_i.div_euclid(TILE),
        (oy_i + n as i64 - 1).div_euclid(TILE),
    );
    let mut svg_tiles = String::new();
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut first_err: Option<String> = None;
    for ty in ty0..=ty1 {
        if ty < 0 || ty >= tiles_per_axis {
            continue;
        }
        for tx in tx0..=tx1 {
            let wx = tx.rem_euclid(tiles_per_axis);
            let (dx, dy) = (tx * TILE - ox_i, ty * TILE - oy_i);
            match tile_bytes(net, &opts.cache_dir, z, wx, ty)
                .and_then(|b| decode_jpeg(&b).map(|d| (b, d)))
            {
                Ok((raw, (w, h, rgb))) => {
                    for yy in 0..h.min(TILE as usize) {
                        let py = dy + yy as i64;
                        if py < 0 || py >= n as i64 {
                            continue;
                        }
                        for xx in 0..w.min(TILE as usize) {
                            let px = dx + xx as i64;
                            if px < 0 || px >= n as i64 {
                                continue;
                            }
                            let si = (yy * w + xx) * 3;
                            let di = (py as usize * n + px as usize) * 3;
                            cv.px[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
                        }
                    }
                    svg_tiles.push_str(&format!(
                        "<image x=\"{dx}\" y=\"{dy}\" width=\"{TILE}\" height=\"{TILE}\" href=\"data:image/jpeg;base64,{}\"/>\n",
                        base64(&raw)
                    ));
                    ok += 1;
                }
                Err(e) => {
                    failed += 1;
                    first_err.get_or_insert_with(|| e.to_string());
                }
            }
        }
    }
    // Overlays.
    let to_img = |la: f64, lo: f64| {
        let (x, y) = world_px(la, lo, z);
        (x - ox_i as f64, y - oy_i as f64)
    };
    for &(mla, mlo) in &opts.markers {
        let (mx, my) = to_img(mla, mlo);
        cv.disc(mx, my, 6.0, [255, 255, 255]);
        cv.disc(mx, my, 4.0, [31, 78, 158]);
    }
    let r_px = (radius_m / m_per_px).max(6.0);
    cv.ring(ccx, ccy, r_px, 2.5, [255, 255, 255]);
    cv.ring(ccx, ccy, r_px, 1.5, [215, 25, 28]);
    let (cxi, cyi) = (ccx.round() as i64, ccy.round() as i64);
    cv.rect(cxi - 13, cyi - 2, 27, 5, [255, 255, 255], 1.0);
    cv.rect(cxi - 2, cyi - 13, 5, 27, [255, 255, 255], 1.0);
    cv.rect(cxi - 12, cyi - 1, 25, 3, [215, 25, 28], 1.0);
    cv.rect(cxi - 1, cyi - 12, 3, 25, [215, 25, 28], 1.0);
    // Label box (top-left).
    let mut lines: Vec<String> = Vec::new();
    if let Some(h) = &opts.headline {
        lines.push(h.clone());
    }
    lines.push(format!("{lat:.6}, {lon:.6}"));
    lines.push(format!("radius {radius_m:.0} m (red circle)"));
    let scale = 2i64;
    let line_h = (GLYPH_H as i64 + 3) * scale;
    let max_chars = ((n as i64 - 60) / ((GLYPH_W as i64 + 1) * scale)) as usize;
    let lines: Vec<String> = lines
        .into_iter()
        .map(|l| l.chars().take(max_chars).collect())
        .collect();
    let box_w = lines
        .iter()
        .map(|l| l.chars().count() as i64)
        .max()
        .unwrap_or(0)
        * (GLYPH_W as i64 + 1)
        * scale
        + 20;
    cv.rect(
        12,
        12,
        box_w,
        line_h * lines.len() as i64 + 14,
        [255, 255, 255],
        0.85,
    );
    for (i, l) in lines.iter().enumerate() {
        let c = if i == 0 && opts.headline.is_some() {
            [15, 42, 61]
        } else {
            [30, 30, 30]
        };
        cv.text(22, 20 + i as i64 * line_h, l, scale, c, None);
    }
    // Scale bar (bottom-left).
    let (len_m, len_px) = nice_scale(m_per_px);
    let (sx, sy) = (24i64, n as i64 - 46);
    cv.rect(
        sx - 10,
        sy - 30,
        len_px.round() as i64 + 20,
        50,
        [255, 255, 255],
        0.85,
    );
    cv.rect(sx, sy, len_px.round() as i64, 5, [20, 20, 20], 1.0);
    cv.rect(sx, sy - 8, 3, 13, [20, 20, 20], 1.0);
    cv.rect(
        sx + len_px.round() as i64 - 3,
        sy - 8,
        3,
        13,
        [20, 20, 20],
        1.0,
    );
    let label = scale_label(len_m);
    cv.text(sx, sy - 26, &label, 2, [20, 20, 20], None);
    // North arrow (top-right).
    let (ax, ay) = (n as f64 - 46.0, 28.0);
    cv.rect(
        ax as i64 - 22,
        ay as i64 - 12,
        44,
        74,
        [255, 255, 255],
        0.85,
    );
    cv.triangle(
        [(ax, ay), (ax - 12.0, ay + 30.0), (ax + 12.0, ay + 30.0)],
        [20, 20, 20],
    );
    cv.text(ax as i64 - 5, ay as i64 + 36, "N", 2, [20, 20, 20], None);
    // Attribution (bottom-right).
    let attr = if ok > 0 {
        "USGS The National Map"
    } else {
        "basemap unavailable (offline)"
    };
    let aw = attr.len() as i64 * (GLYPH_W as i64 + 1);
    cv.rect(
        n as i64 - aw - 16,
        n as i64 - 20,
        aw + 12,
        16,
        [255, 255, 255],
        0.8,
    );
    cv.text(
        n as i64 - aw - 10,
        n as i64 - 16,
        attr,
        1,
        [60, 60, 60],
        None,
    );

    let stem = if opts.stem.is_empty() {
        "map"
    } else {
        &opts.stem
    };
    let png_path = out_dir.join(format!("{stem}.png"));
    cv.write_png(&png_path)?;

    // SVG twin.
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{n}\" height=\"{n}\" viewBox=\"0 0 {n} {n}\">\n\
         <defs><pattern id=\"grid\" width=\"64\" height=\"64\" patternUnits=\"userSpaceOnUse\"><rect width=\"64\" height=\"64\" fill=\"#e8ecef\"/>\
         <path d=\"M64 0H0V64\" fill=\"none\" stroke=\"#cdd4db\"/></pattern></defs>\n\
         <rect width=\"{n}\" height=\"{n}\" fill=\"url(#grid)\"/>\n{svg_tiles}"
    );
    for &(mla, mlo) in &opts.markers {
        let (mx, my) = to_img(mla, mlo);
        svg.push_str(&format!("<circle cx=\"{mx:.1}\" cy=\"{my:.1}\" r=\"5\" fill=\"#1f4e9e\" stroke=\"#fff\" stroke-width=\"2\"/>\n"));
    }
    svg.push_str(&format!(
        "<circle cx=\"{ccx:.1}\" cy=\"{ccy:.1}\" r=\"{r_px:.1}\" fill=\"none\" stroke=\"#fff\" stroke-width=\"6\"/>\n\
         <circle cx=\"{ccx:.1}\" cy=\"{ccy:.1}\" r=\"{r_px:.1}\" fill=\"none\" stroke=\"#d7191c\" stroke-width=\"3\"/>\n\
         <path d=\"M{x0:.1} {ccy:.1}H{x1:.1}M{ccx:.1} {y0:.1}V{y1:.1}\" stroke=\"#fff\" stroke-width=\"5\"/>\n\
         <path d=\"M{x0:.1} {ccy:.1}H{x1:.1}M{ccx:.1} {y0:.1}V{y1:.1}\" stroke=\"#d7191c\" stroke-width=\"3\"/>\n",
        x0 = ccx - 12.0,
        x1 = ccx + 12.0,
        y0 = ccy - 12.0,
        y1 = ccy + 12.0
    ));
    let font = "font-family=\"Helvetica, Arial, sans-serif\"";
    svg.push_str(&format!(
        "<rect x=\"12\" y=\"12\" width=\"{bw}\" height=\"{bh}\" fill=\"#fff\" fill-opacity=\"0.85\"/>\n",
        bw = box_w,
        bh = line_h * lines.len() as i64 + 14
    ));
    for (i, l) in lines.iter().enumerate() {
        svg.push_str(&format!(
            "<text x=\"22\" y=\"{}\" {font} font-size=\"17\" fill=\"#0f2a3d\"{}>{}</text>\n",
            34 + i as i64 * line_h,
            if i == 0 && opts.headline.is_some() {
                " font-weight=\"bold\""
            } else {
                ""
            },
            esc(l)
        ));
    }
    svg.push_str(&format!(
        "<rect x=\"{}\" y=\"{}\" width=\"{:.0}\" height=\"50\" fill=\"#fff\" fill-opacity=\"0.85\"/>\n\
         <rect x=\"{sx}\" y=\"{sy}\" width=\"{len_px:.1}\" height=\"5\" fill=\"#141414\"/>\n\
         <text x=\"{sx}\" y=\"{}\" {font} font-size=\"16\" fill=\"#141414\">{}</text>\n\
         <rect x=\"{}\" y=\"{}\" width=\"44\" height=\"74\" fill=\"#fff\" fill-opacity=\"0.85\"/>\n\
         <polygon points=\"{ax},{ay} {},{} {},{}\" fill=\"#141414\"/>\n\
         <text x=\"{}\" y=\"{}\" {font} font-size=\"18\" font-weight=\"bold\" text-anchor=\"middle\">N</text>\n\
         <text x=\"{}\" y=\"{}\" {font} font-size=\"11\" fill=\"#3c3c3c\" text-anchor=\"end\">{}</text>\n</svg>\n",
        sx - 10,
        sy - 30,
        len_px + 20.0,
        sy - 10,
        esc(&label),
        ax - 22.0,
        ay - 12.0,
        ax - 12.0,
        ay + 30.0,
        ax + 12.0,
        ay + 30.0,
        ax,
        ay + 52.0,
        n - 8,
        n - 8,
        esc(attr)
    ));
    let svg_path = out_dir.join(format!("{stem}.svg"));
    std::fs::write(&svg_path, svg)?;
    if ok == 0
        && let Some(e) = &first_err
    {
        // Not fatal: the overlays still carry the answer.
        eprintln!("[fdop-geo] static map without basemap: {e}");
    }
    Ok(StaticMap {
        png: png_path,
        svg: svg_path,
        zoom: z,
        m_per_px,
        radius_px: r_px,
        tiles_ok: ok,
        tiles_failed: failed,
        offline: ok == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_and_projection() {
        // 300 m radius at 40.9N: ~1.8 m/px target -> z = 16 (1.83 m/px).
        assert_eq!(choose_zoom(40.876, 300.0, 1024, 16), 16);
        assert_eq!(choose_zoom(40.876, 30_000.0, 1024, 16), 9);
        let (x, y) = world_px(0.0, 0.0, 0);
        assert!((x - 128.0).abs() < 1e-9 && (y - 128.0).abs() < 1e-9);
        let (len, px) = nice_scale(1.83);
        assert!((100.0..=280.0).contains(&px), "{len} {px}");
        assert_eq!(scale_label(2000.0), "2 km");
    }

    #[test]
    fn offline_render_writes_files() {
        let dir = std::env::temp_dir().join(format!("fdop-geo-map-{}", std::process::id()));
        let opts = StaticMapOptions {
            cache_dir: dir.join("cache"),
            size_px: 512,
            headline: Some("Test Place, NY 11771".into()),
            markers: vec![(40.88, -73.52)],
            ..Default::default()
        };
        let m = static_map(None, 40.876466, -73.524951, 300.0, &dir, &opts).unwrap();
        assert!(m.offline);
        let svg = std::fs::read_to_string(&m.svg).unwrap();
        assert!(svg.contains("stroke=\"#d7191c\""));
        assert!(svg.contains("40.876466, -73.524951"));
        assert!(std::fs::metadata(&m.png).unwrap().len() > 1000);
    }
}
