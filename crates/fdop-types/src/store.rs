//! Compact on-disk store of filtered ADS-B tracks (`.fdtrk`, gzip-compressed binary).
//!
//! Layout (inside gzip): magic `FDOPTRK1`, u32 meta-JSON length, meta JSON, u32 leg count,
//! then per leg: five u16-length-prefixed UTF-8 strings (key, icao, reg, type, source),
//! u8 engine code, u32 point count, and packed little-endian points (see [`TrackPoint`]).

use crate::BBox;
use crate::engine::EngineClass;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"FDOPTRK1";
const POINT_BYTES: usize = 8 * 3 + 4 * 4 + 2;

/// Altitude provenance of a point.
pub mod alt_src {
    /// GNSS geometric height converted to MSL with the geoid undulation.
    pub const GEOM: u8 = 0;
    /// Barometric altitude corrected by the leg's (or region's) measured geom-baro offset.
    pub const BARO_CORRECTED: u8 = 1;
    /// Raw barometric pressure altitude (no offset available).
    pub const BARO_RAW: u8 = 2;
}

/// Point flags.
pub mod pt_flags {
    /// Position from multilateration.
    pub const MLAT: u8 = 1;
    /// Position from TIS-B.
    pub const TISB: u8 = 2;
    /// readsb marked the position stale / estimated.
    pub const STALE: u8 = 4;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackPoint {
    /// Unix seconds (UTC).
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    /// Height above mean sea level (m); provenance in `alt_src`.
    pub alt_m: f32,
    /// Ground speed (m/s), NaN when not reported.
    pub gs_mps: f32,
    /// True track over ground (degrees), NaN when not reported.
    pub track_deg: f32,
    /// Vertical rate (m/s, positive up; geometric preferred), NaN when not reported.
    pub vrate_mps: f32,
    pub alt_src: u8,
    pub flags: u8,
}

#[derive(Clone, Debug)]
pub struct FlightLeg {
    /// Unique key within a store, e.g. `a1b2c3_20260908_0`.
    pub key: String,
    pub icao: String,
    pub reg: String,
    /// ICAO type designator ("" when unknown).
    pub typ: String,
    /// Feed(s) the points came from, e.g. "prod", "mlat", "prod+mlat".
    pub source: String,
    pub engine: EngineClass,
    /// Time-sorted points.
    pub points: Vec<TrackPoint>,
}

impl FlightLeg {
    pub fn t0(&self) -> f64 {
        self.points.first().map_or(f64::NAN, |p| p.t)
    }
    pub fn t1(&self) -> f64 {
        self.points.last().map_or(f64::NAN, |p| p.t)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoreMeta {
    pub version: u32,
    /// Spatial filter applied at ingest (already padded).
    pub bbox: BBox,
    /// Time filter applied at ingest (unix s, already padded).
    pub t_start: f64,
    pub t_end: f64,
    /// Geoid undulation N (m) used to convert GNSS heights to MSL (H = h - N).
    pub geoid_n_m: f64,
    /// Release tags / files ingested.
    pub sources: Vec<String>,
    /// Free-form ingest statistics (counts per engine class, dropped jets, ...).
    pub stats: serde_json::Value,
    pub created_unix: f64,
}

#[derive(Clone, Debug)]
pub struct TrackStore {
    pub meta: StoreMeta,
    pub legs: Vec<FlightLeg>,
}

fn w_str<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    let b = s.as_bytes();
    let n = b.len().min(u16::MAX as usize);
    w.write_all(&(n as u16).to_le_bytes())?;
    w.write_all(&b[..n])
}

fn r_exact<R: Read, const N: usize>(r: &mut R) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn r_str<R: Read>(r: &mut R) -> io::Result<String> {
    let n = u16::from_le_bytes(r_exact::<_, 2>(r)?) as usize;
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    String::from_utf8(b).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

impl TrackStore {
    pub fn n_points(&self) -> usize {
        self.legs.iter().map(|l| l.points.len()).sum()
    }

    /// Write atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("fdtrk.part");
        {
            let f = std::fs::File::create(&tmp)?;
            let mut w = GzEncoder::new(BufWriter::with_capacity(1 << 20, f), Compression::fast());
            w.write_all(MAGIC)?;
            let meta = serde_json::to_vec(&self.meta).map_err(io::Error::other)?;
            w.write_all(&(meta.len() as u32).to_le_bytes())?;
            w.write_all(&meta)?;
            w.write_all(&(self.legs.len() as u32).to_le_bytes())?;
            let mut buf = Vec::with_capacity(POINT_BYTES * 4096);
            for leg in &self.legs {
                for s in [&leg.key, &leg.icao, &leg.reg, &leg.typ, &leg.source] {
                    w_str(&mut w, s)?;
                }
                w.write_all(&[leg.engine.code()])?;
                w.write_all(&(leg.points.len() as u32).to_le_bytes())?;
                buf.clear();
                for p in &leg.points {
                    buf.extend_from_slice(&p.t.to_le_bytes());
                    buf.extend_from_slice(&p.lat.to_le_bytes());
                    buf.extend_from_slice(&p.lon.to_le_bytes());
                    buf.extend_from_slice(&p.alt_m.to_le_bytes());
                    buf.extend_from_slice(&p.gs_mps.to_le_bytes());
                    buf.extend_from_slice(&p.track_deg.to_le_bytes());
                    buf.extend_from_slice(&p.vrate_mps.to_le_bytes());
                    buf.push(p.alt_src);
                    buf.push(p.flags);
                }
                w.write_all(&buf)?;
            }
            let inner = w.finish()?;
            inner.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        let mut r = BufReader::with_capacity(1 << 20, GzDecoder::new(BufReader::new(f)));
        let magic = r_exact::<_, 8>(&mut r)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not an fdop track store", path.display()),
            ));
        }
        let mlen = u32::from_le_bytes(r_exact::<_, 4>(&mut r)?) as usize;
        let mut mb = vec![0u8; mlen];
        r.read_exact(&mut mb)?;
        let meta: StoreMeta = serde_json::from_slice(&mb).map_err(io::Error::other)?;
        let n_legs = u32::from_le_bytes(r_exact::<_, 4>(&mut r)?) as usize;
        let mut legs = Vec::with_capacity(n_legs);
        let mut pb = Vec::new();
        for _ in 0..n_legs {
            let key = r_str(&mut r)?;
            let icao = r_str(&mut r)?;
            let reg = r_str(&mut r)?;
            let typ = r_str(&mut r)?;
            let source = r_str(&mut r)?;
            let engine = EngineClass::from_code(r_exact::<_, 1>(&mut r)?[0]);
            let n = u32::from_le_bytes(r_exact::<_, 4>(&mut r)?) as usize;
            pb.resize(n * POINT_BYTES, 0);
            r.read_exact(&mut pb)?;
            let mut points = Vec::with_capacity(n);
            let (rows, _) = pb.as_chunks::<POINT_BYTES>();
            for c in rows {
                let f64_at = |o: usize| f64::from_le_bytes(c[o..o + 8].try_into().unwrap());
                let f32_at = |o: usize| f32::from_le_bytes(c[o..o + 4].try_into().unwrap());
                points.push(TrackPoint {
                    t: f64_at(0),
                    lat: f64_at(8),
                    lon: f64_at(16),
                    alt_m: f32_at(24),
                    gs_mps: f32_at(28),
                    track_deg: f32_at(32),
                    vrate_mps: f32_at(36),
                    alt_src: c[40],
                    flags: c[41],
                });
            }
            legs.push(FlightLeg {
                key,
                icao,
                reg,
                typ,
                source,
                engine,
                points,
            });
        }
        Ok(Self { meta, legs })
    }

    /// Keep only points inside `bbox` and `[t0, t1]`, plus two neighbours on each side of every
    /// kept run (so interpolation at the edges stays anchored). Legs left with < 2 points go.
    pub fn filtered(&self, bbox: &BBox, t0: f64, t1: f64) -> TrackStore {
        let mut legs = Vec::new();
        for leg in &self.legs {
            let keep: Vec<bool> = leg
                .points
                .iter()
                .map(|p| p.t >= t0 && p.t <= t1 && bbox.contains(p.lat, p.lon))
                .collect();
            let n = keep.len();
            let mut grown = vec![false; n];
            for (i, _) in keep.iter().enumerate().filter(|(_, k)| **k) {
                for g in grown
                    .iter_mut()
                    .take((i + 3).min(n))
                    .skip(i.saturating_sub(2))
                {
                    *g = true;
                }
            }
            let pts: Vec<TrackPoint> = leg
                .points
                .iter()
                .zip(&grown)
                .filter(|(_, g)| **g)
                .map(|(p, _)| *p)
                .collect();
            if pts.len() >= 2 {
                legs.push(FlightLeg {
                    points: pts,
                    ..leg.clone()
                });
            }
        }
        let mut meta = self.meta.clone();
        meta.bbox = *bbox;
        meta.t_start = t0;
        meta.t_end = t1;
        TrackStore { meta, legs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = |t: f64| TrackPoint {
            t,
            lat: 40.0 + t * 1e-5,
            lon: -73.0,
            alt_m: 1500.0,
            gs_mps: f32::NAN,
            track_deg: 90.0,
            vrate_mps: 0.0,
            alt_src: alt_src::GEOM,
            flags: pt_flags::MLAT,
        };
        let store = TrackStore {
            meta: StoreMeta {
                version: 1,
                bbox: BBox::new(39.0, -74.0, 41.0, -72.0),
                t_start: 0.0,
                t_end: 100.0,
                geoid_n_m: -32.0,
                sources: vec!["test".into()],
                stats: serde_json::json!({"kept": 1}),
                created_unix: 0.0,
            },
            legs: vec![FlightLeg {
                key: "abc123_0".into(),
                icao: "abc123".into(),
                reg: "N1".into(),
                typ: "PC12".into(),
                source: "prod".into(),
                engine: EngineClass::Turboprop,
                points: (0..10).map(|i| p(i as f64 * 10.0)).collect(),
            }],
        };
        let dir = std::env::temp_dir().join(format!("fdop-store-test-{}", std::process::id()));
        let path = dir.join("t.fdtrk");
        store.save(&path).unwrap();
        let back = TrackStore::load(&path).unwrap();
        assert_eq!(back.legs.len(), 1);
        assert_eq!(back.legs[0].points.len(), 10);
        assert_eq!(back.legs[0].typ, "PC12");
        assert!(back.legs[0].points[3].gs_mps.is_nan());
        assert_eq!(back.legs[0].points[3].t, 30.0);
        let f = back.filtered(&BBox::new(39.0, -74.0, 41.0, -72.0), 35.0, 45.0);
        assert_eq!(f.legs[0].points.len(), 5); // t=40 plus two neighbours each side
    }
}
