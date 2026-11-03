//! Band-limited STFT spectrograms (frankenscipy `fsci_fft::rfft`), row-median normalisation,
//! bilinear sampling, binary save/load and PNG rendering with curve overlays.

use fsci_fft::{FftOptions, WorkerPolicy};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecMeta {
    pub sr: u32,
    pub win_s: f64,
    pub hop_s: f64,
    pub nfft: usize,
    pub df: f64,
    pub fmin: f64,
    pub fmax: f64,
    /// How rows were normalised (e.g. "row-median over [t0,t1]").
    pub norm: String,
}

/// Spectrogram in dB, row-major `[f][t]` (one row per frequency bin).
#[derive(Clone, Debug)]
pub struct Spec {
    pub t: Vec<f64>,
    pub f: Vec<f64>,
    pub db: Vec<f32>,
    pub meta: SpecMeta,
}

impl Spec {
    pub fn nt(&self) -> usize {
        self.t.len()
    }
    pub fn nf(&self) -> usize {
        self.f.len()
    }
    #[inline]
    pub fn at(&self, fi: usize, ti: usize) -> f32 {
        self.db[fi * self.t.len() + ti]
    }
    pub fn row(&self, fi: usize) -> &[f32] {
        let nt = self.t.len();
        &self.db[fi * nt..(fi + 1) * nt]
    }
    pub fn dt(&self) -> f64 {
        if self.t.len() > 1 {
            self.t[1] - self.t[0]
        } else {
            self.meta.hop_s
        }
    }
    pub fn df(&self) -> f64 {
        if self.f.len() > 1 {
            self.f[1] - self.f[0]
        } else {
            self.meta.df
        }
    }

    /// Bilinear sample at (t, f); `None` outside the grid.
    #[inline]
    pub fn bilinear(&self, tq: f64, fq: f64) -> Option<f32> {
        let (nt, nf) = (self.t.len(), self.f.len());
        if nt < 2 || nf < 2 {
            return None;
        }
        let ti = (tq - self.t[0]) / self.dt();
        let fi = (fq - self.f[0]) / self.df();
        if !(ti >= 0.0 && fi >= 0.0 && ti <= (nt - 1) as f64 && fi <= (nf - 1) as f64) {
            return None;
        }
        let ti = ti.min(nt as f64 - 1.000_001);
        let fi = fi.min(nf as f64 - 1.000_001);
        let (t0, f0) = (ti as usize, fi as usize);
        let (wt, wf) = ((ti - t0 as f64) as f32, (fi - f0 as f64) as f32);
        let a = self.db[f0 * nt + t0];
        let b = self.db[f0 * nt + t0 + 1];
        let c = self.db[(f0 + 1) * nt + t0];
        let d = self.db[(f0 + 1) * nt + t0 + 1];
        Some(a * (1.0 - wt) * (1.0 - wf) + b * wt * (1.0 - wf) + c * (1.0 - wt) * wf + d * wt * wf)
    }

    /// Subtract each row's median over frames `[a, b)` (whole row when `None`): stationary hum
    /// and the noise floor go to ~0 dB.
    pub fn normalize_rows(&mut self, frames: Option<(usize, usize)>) {
        let nt = self.t.len();
        let (a, b) = frames.unwrap_or((0, nt));
        let (a, b) = (a.min(nt), b.min(nt).max(a.min(nt) + 1).min(nt));
        self.db.par_chunks_mut(nt).for_each(|row| {
            let mut tmp: Vec<f32> = row[a..b].to_vec();
            let m = median_f32(&mut tmp);
            for v in row.iter_mut() {
                *v -= m;
            }
        });
        self.meta.norm = format!(
            "row-median over t=[{:.1},{:.1}]",
            self.t[a],
            self.t[b.saturating_sub(1)]
        );
    }

    /// Sub-spectrogram over frames `[a, b)` and bins `[fa, fb)`.
    pub fn crop(&self, a: usize, b: usize, fa: usize, fb: usize) -> Spec {
        let nt = self.t.len();
        let (b, fb) = (b.min(nt), fb.min(self.f.len()));
        let mut db = Vec::with_capacity((b - a) * (fb - fa));
        for fi in fa..fb {
            db.extend_from_slice(&self.db[fi * nt + a..fi * nt + b]);
        }
        Spec {
            t: self.t[a..b].to_vec(),
            f: self.f[fa..fb].to_vec(),
            db,
            meta: self.meta.clone(),
        }
    }

    pub fn frame_index(&self, t: f64) -> usize {
        (((t - self.t[0]) / self.dt()).round().max(0.0) as usize)
            .min(self.t.len().saturating_sub(1))
    }
    pub fn bin_index(&self, f: f64) -> usize {
        (((f - self.f[0]) / self.df()).round().max(0.0) as usize)
            .min(self.f.len().saturating_sub(1))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        w.write_all(b"FDSPEC01")?;
        let meta = serde_json::to_vec(&self.meta).map_err(std::io::Error::other)?;
        w.write_all(&(meta.len() as u32).to_le_bytes())?;
        w.write_all(&meta)?;
        w.write_all(&(self.t.len() as u64).to_le_bytes())?;
        w.write_all(&(self.f.len() as u64).to_le_bytes())?;
        for v in &self.t {
            w.write_all(&v.to_le_bytes())?;
        }
        for v in &self.f {
            w.write_all(&v.to_le_bytes())?;
        }
        for v in &self.db {
            w.write_all(&v.to_le_bytes())?;
        }
        w.flush()
    }

    pub fn load(path: &Path) -> std::io::Result<Spec> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != b"FDSPEC01" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not an fdop spectrogram",
            ));
        }
        let mut b4 = [0u8; 4];
        r.read_exact(&mut b4)?;
        let mut mb = vec![0u8; u32::from_le_bytes(b4) as usize];
        r.read_exact(&mut mb)?;
        let meta: SpecMeta = serde_json::from_slice(&mb).map_err(std::io::Error::other)?;
        let mut b8 = [0u8; 8];
        r.read_exact(&mut b8)?;
        let nt = u64::from_le_bytes(b8) as usize;
        r.read_exact(&mut b8)?;
        let nf = u64::from_le_bytes(b8) as usize;
        let mut rd_f64 = |n: usize| -> std::io::Result<Vec<f64>> {
            let mut buf = vec![0u8; n * 8];
            r.read_exact(&mut buf)?;
            Ok(buf
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_le_bytes(*c))
                .collect())
        };
        let t = rd_f64(nt)?;
        let f = rd_f64(nf)?;
        let mut buf = vec![0u8; nt * nf * 4];
        r.read_exact(&mut buf)?;
        let db = buf
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        Ok(Spec { t, f, db, meta })
    }
}

pub fn median_f32(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    *m
}

pub fn median_f64(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let k = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    *m
}

/// Periodic Hann window (scipy `get_window("hann", n)`).
pub fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
        .collect()
}

/// Hann STFT of `x` (sample rate `sr`), zero-padded so bins are <= `df_max` Hz, restricted to
/// `[fmin, fmax]`. Frame centres are `t_offset + (i*hop + nwin/2)/sr`. Returns raw dB
/// (power / (sum w)^2); call [`Spec::normalize_rows`] afterwards.
#[allow(clippy::too_many_arguments)]
pub fn stft_band_db(
    x: &[f32],
    sr: u32,
    win_s: f64,
    hop_s: f64,
    fmin: f64,
    fmax: f64,
    df_max: f64,
    t_offset: f64,
) -> Result<Spec, String> {
    let srf = sr as f64;
    let nwin = (win_s * srf).round() as usize;
    let hop = ((hop_s * srf).round() as usize).max(1);
    if nwin < 8 || x.len() < nwin {
        return Err(format!(
            "audio ({} samples) shorter than one {win_s} s window",
            x.len()
        ));
    }
    let nfft = nwin.max((srf / df_max).ceil() as usize).next_power_of_two();
    let df = srf / nfft as f64;
    let b0 = (fmin / df).ceil() as usize;
    let b1 = ((fmax / df).floor() as usize).min(nfft / 2);
    if b1 <= b0 + 1 {
        return Err("empty frequency band".into());
    }
    let nb = b1 - b0 + 1;
    let nfr = (x.len() - nwin) / hop + 1;
    let w = hann(nwin);
    let wsum: f64 = w.iter().sum();
    let norm = wsum * wsum;
    let opts = FftOptions::default().with_workers(WorkerPolicy::Exact(1));
    // Column-major work buffer [t][f], transposed to row-major at the end.
    let cols: Vec<Vec<f32>> = (0..nfr)
        .into_par_iter()
        .map_init(
            || vec![0.0f64; nfft],
            |buf, i| {
                let s = i * hop;
                for (k, b) in buf.iter_mut().enumerate() {
                    *b = if k < nwin {
                        x[s + k] as f64 * w[k]
                    } else {
                        0.0
                    };
                }
                let spec = fsci_fft::rfft(buf, &opts).unwrap_or_default();
                (b0..=b1)
                    .map(|k| {
                        let (re, im) = spec.get(k).copied().unwrap_or((0.0, 0.0));
                        (10.0 * ((re * re + im * im) / norm + 1e-20).log10()) as f32
                    })
                    .collect()
            },
        )
        .collect();
    drain_fft_traces();
    let mut db = vec![0f32; nb * nfr];
    db.par_chunks_mut(nfr).enumerate().for_each(|(fi, row)| {
        for (ti, v) in row.iter_mut().enumerate() {
            *v = cols[ti][fi];
        }
    });
    let t = (0..nfr)
        .map(|i| t_offset + (i * hop) as f64 / srf + nwin as f64 / (2.0 * srf))
        .collect();
    let f = (b0..=b1).map(|k| k as f64 * df).collect();
    Ok(Spec {
        t,
        f,
        db,
        meta: SpecMeta {
            sr,
            win_s,
            hop_s: hop as f64 / srf,
            nfft,
            df,
            fmin,
            fmax,
            norm: "raw dB".into(),
        },
    })
}

/// fsci-fft appends a trace record per transform to a global log; drain it so long runs do
/// not accumulate memory.
pub fn drain_fft_traces() {
    let _ = fsci_fft::take_transform_traces();
}

/// A curve to draw over a spectrogram image.
pub struct Overlay {
    pub points: Vec<(f64, f64)>,
    pub color: [u8; 3],
    /// Draw as dots (trace samples) rather than a polyline (model curves).
    pub dots: bool,
}

fn colormap(x: f32) -> [u8; 3] {
    // perceptual "inferno"-like ramp: black -> purple -> orange -> pale yellow
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.0, 0.0, 4.0]),
        (0.2, [40.0, 11.0, 84.0]),
        (0.4, [101.0, 21.0, 110.0]),
        (0.6, [188.0, 55.0, 84.0]),
        (0.8, [249.0, 142.0, 9.0]),
        (1.0, [252.0, 255.0, 164.0]),
    ];
    let x = x.clamp(0.0, 1.0);
    for w in STOPS.windows(2) {
        let (a, b) = (w[0], w[1]);
        if x <= b.0 {
            let u = (x - a.0) / (b.0 - a.0);
            return [
                (a.1[0] + u * (b.1[0] - a.1[0])) as u8,
                (a.1[1] + u * (b.1[1] - a.1[1])) as u8,
                (a.1[2] + u * (b.1[2] - a.1[2])) as u8,
            ];
        }
    }
    [252, 255, 164]
}

/// Axis decoration for [`render_png_axes`].
#[derive(Clone, Debug, Default)]
pub struct Axes {
    /// Drawn above the plot.
    pub title: String,
    /// Y ticks as (value in `spec.f` units, label); `None` = automatic ticks on `spec.f` in Hz.
    pub y_ticks: Option<Vec<(f64, String)>>,
}

/// Render to PNG (low frequencies at the bottom). Large spectrograms are max-pooled down to at
/// most `max_w` x `max_h` pixels. dB is mapped linearly over [vmin, vmax].
pub fn render_png(
    spec: &Spec,
    path: &Path,
    overlays: &[Overlay],
    vmin: f32,
    vmax: f32,
    max_w: usize,
    max_h: usize,
) -> Result<(), String> {
    if spec.nt() == 0 || spec.nf() == 0 {
        return Err("empty spectrogram".into());
    }
    let (w, h, img) = render_rgb(spec, overlays, vmin, vmax, max_w, max_h);
    write_png_rgb(path, w, h, &img)
}

/// [`render_png`] plus a title, a recording-time axis (m:ss) and a frequency axis.
#[allow(clippy::too_many_arguments)]
pub fn render_png_axes(
    spec: &Spec,
    path: &Path,
    overlays: &[Overlay],
    vmin: f32,
    vmax: f32,
    max_w: usize,
    max_h: usize,
    axes: &Axes,
) -> Result<(), String> {
    if spec.nt() == 0 || spec.nf() == 0 {
        return Err("empty spectrogram".into());
    }
    let (w, h, plot) = render_rgb(spec, overlays, vmin, vmax, max_w, max_h);
    let (sc, gw, gh) = (2usize, fdop_types::font::GLYPH_W, fdop_types::font::GLYPH_H);
    let (left, right, top, bottom) = (
        14 + 6 * (gw + 1) * sc,
        12,
        10 + gh * sc + 8,
        12 + gh * sc + 8,
    );
    let (cw, ch) = (w + left + right, h + top + bottom);
    let bg = [22u8, 22, 28];
    let fg = [225u8, 225, 230];
    let mut img: Vec<u8> = bg.iter().copied().cycle().take(cw * ch * 3).collect();
    for y in 0..h {
        let d = ((y + top) * cw + left) * 3;
        img[d..d + w * 3].copy_from_slice(&plot[y * w * 3..(y + 1) * w * 3]);
    }
    let mut cv = Canvas {
        w: cw,
        h: ch,
        rgb: &mut img,
    };
    cv.text(left as i64, 8, &axes.title, sc, fg);
    // time axis
    let (t0, t1) = (spec.t[0], spec.t[spec.nt() - 1]);
    let step = nice_step(
        (t1 - t0) / 9.0,
        &[
            1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0,
        ],
    );
    let mut tk = (t0 / step).ceil() * step;
    while tk <= t1 + 1e-9 {
        let x = left as i64 + ((tk - t0) / (t1 - t0).max(1e-9) * (w - 1) as f64).round() as i64;
        let y = (top + h) as i64;
        cv.vline(x, y, y + 5, fg);
        let s = mmss(tk);
        let tw = (s.chars().count() * (gw + 1) * sc) as i64;
        cv.text(x - tw / 2, y + 8, &s, sc, fg);
        tk += step;
    }
    // frequency axis
    let (f0, f1) = (spec.f[0], spec.f[spec.nf() - 1]);
    let ticks = axes.y_ticks.clone().unwrap_or_else(|| {
        let st = nice_step(
            (f1 - f0) / 6.0,
            &[
                0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0,
            ],
        );
        let mut v = Vec::new();
        let mut fk = (f0 / st).ceil() * st;
        while fk <= f1 + 1e-9 {
            v.push((
                fk,
                if st < 1.0 {
                    format!("{fk:.1}")
                } else {
                    format!("{fk:.0}")
                },
            ));
            fk += st;
        }
        v
    });
    for (fv, lab) in ticks {
        if !(f0..=f1).contains(&fv) {
            continue;
        }
        let y =
            top as i64 + ((1.0 - (fv - f0) / (f1 - f0).max(1e-12)) * (h - 1) as f64).round() as i64;
        cv.hline(left as i64 - 6, left as i64 - 1, y, fg);
        let tw = (lab.chars().count() * (gw + 1) * sc) as i64;
        cv.text(
            left as i64 - 9 - tw,
            y - (gh * sc / 2) as i64 + 1,
            &lab,
            sc,
            fg,
        );
    }
    cv.text(4, 8, "Hz", sc, fg);
    write_png_rgb(path, cw, ch, &img)
}

struct Canvas<'a> {
    w: usize,
    h: usize,
    rgb: &'a mut [u8],
}

impl Canvas<'_> {
    fn put(&mut self, x: i64, y: i64, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            let k = (y as usize * self.w + x as usize) * 3;
            self.rgb[k..k + 3].copy_from_slice(&c);
        }
    }
    fn vline(&mut self, x: i64, y0: i64, y1: i64, c: [u8; 3]) {
        for y in y0..=y1 {
            self.put(x, y, c);
        }
    }
    fn hline(&mut self, x0: i64, x1: i64, y: i64, c: [u8; 3]) {
        for x in x0..=x1 {
            self.put(x, y, c);
        }
    }
    fn text(&mut self, x: i64, y: i64, s: &str, scale: usize, c: [u8; 3]) {
        let adv = ((fdop_types::font::GLYPH_W + 1) * scale) as i64;
        for (i, ch) in s.chars().enumerate() {
            for gy in 0..fdop_types::font::GLYPH_H {
                for gx in 0..fdop_types::font::GLYPH_W {
                    if fdop_types::font::pixel(ch, gx, gy) {
                        for dy in 0..scale as i64 {
                            for dx in 0..scale as i64 {
                                self.put(
                                    x + i as i64 * adv + (gx * scale) as i64 + dx,
                                    y + (gy * scale) as i64 + dy,
                                    c,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Smallest step from `steps` that is at least `raw` (the last one if none is).
fn nice_step(raw: f64, steps: &[f64]) -> f64 {
    steps
        .iter()
        .copied()
        .find(|&s| s >= raw)
        .unwrap_or(steps[steps.len() - 1])
}

/// Recording time as m:ss (h:mm:ss past an hour).
pub fn mmss(t: f64) -> String {
    let s = t.max(0.0).round() as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Pixels of the plot area: max-pooled down to `max_w` x `max_h`, and stretched (nearest) to at
/// least 360 px tall so that narrow frequency bands stay readable.
fn render_rgb(
    spec: &Spec,
    overlays: &[Overlay],
    vmin: f32,
    vmax: f32,
    max_w: usize,
    max_h: usize,
) -> (usize, usize, Vec<u8>) {
    let (nt, nf) = (spec.nt(), spec.nf());
    let w = nt.min(max_w).max(1);
    let h = nf.max(max_h.min(360)).min(max_h).max(1);
    let mut img = vec![0u8; w * h * 3];
    for py in 0..h {
        let f_lo = (h - 1 - py) * nf / h;
        let f_hi = ((h - py) * nf / h).max(f_lo + 1);
        for px in 0..w {
            let t_lo = px * nt / w;
            let t_hi = ((px + 1) * nt / w).max(t_lo + 1);
            let mut m = f32::NEG_INFINITY;
            for fi in f_lo..f_hi {
                for ti in t_lo..t_hi {
                    m = m.max(spec.at(fi, ti));
                }
            }
            let c = colormap((m - vmin) / (vmax - vmin));
            img[(py * w + px) * 3..(py * w + px) * 3 + 3].copy_from_slice(&c);
        }
    }
    let (t0, t1) = (spec.t[0], spec.t[nt - 1]);
    let (f0, f1) = (spec.f[0], spec.f[nf - 1]);
    let to_px = |t: f64, f: f64| -> Option<(i64, i64)> {
        if !(t.is_finite() && f.is_finite()) || t < t0 || t > t1 || f < f0 || f > f1 {
            return None;
        }
        let x = ((t - t0) / (t1 - t0) * (w - 1) as f64).round() as i64;
        let y = ((1.0 - (f - f0) / (f1 - f0)) * (h - 1) as f64).round() as i64;
        Some((x, y))
    };
    let mut put = |x: i64, y: i64, c: [u8; 3]| {
        if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
            let k = (y as usize * w + x as usize) * 3;
            img[k..k + 3].copy_from_slice(&c);
        }
    };
    for ov in overlays {
        let mut prev: Option<(i64, i64)> = None;
        for &(t, f) in &ov.points {
            let p = to_px(t, f);
            match (p, ov.dots) {
                (Some((x, y)), true) => {
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (-1, 0), (0, -1)] {
                        put(x + dx, y + dy, ov.color);
                    }
                }
                (Some((x, y)), false) => {
                    if let Some((px, py)) = prev {
                        let n = (x - px).abs().max((y - py).abs()).max(1);
                        for s in 0..=n {
                            let xx = px + (x - px) * s / n;
                            let yy = py + (y - py) * s / n;
                            put(xx, yy, ov.color);
                            put(xx, yy + 1, ov.color);
                        }
                    }
                    prev = Some((x, y));
                }
                (None, _) => prev = None,
            }
        }
    }
    (w, h, img)
}

pub fn write_png_rgb(path: &Path, w: usize, h: usize, rgb: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgb).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_lands_in_right_bin_and_normalises() {
        let sr = 4000u32;
        let x: Vec<f32> = (0..sr as usize * 20)
            .map(|i| (2.0 * std::f64::consts::PI * 123.4 * i as f64 / sr as f64).sin() as f32)
            .collect();
        let mut s = stft_band_db(&x, sr, 1.0, 0.25, 40.0, 700.0, 0.25, 0.0).unwrap();
        assert_eq!(s.meta.nfft, 16384);
        let mid = s.nt() / 2;
        let (mut best, mut bf) = (f32::MIN, 0.0);
        for fi in 0..s.nf() {
            if s.at(fi, mid) > best {
                best = s.at(fi, mid);
                bf = s.f[fi];
            }
        }
        assert!((bf - 123.4).abs() < 0.3, "raw peak at {bf}");
        s.normalize_rows(None);
        let v = s.bilinear(s.t[mid], 123.4).unwrap();
        assert!(v.abs() < 1.0, "row-normalised constant tone ~0 dB, got {v}");
        assert!((s.t[0] - 0.5).abs() < 1e-12);
    }
}
