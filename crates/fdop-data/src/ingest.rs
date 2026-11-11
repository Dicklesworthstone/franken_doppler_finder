//! Streaming ingest of readsb `trace_full_*.json` members (adsb.lol `globe_history` tars).
//!
//! The tar is walked sequentially on one thread (it is a network stream); member bytes are
//! batched and decoded + parsed on the rayon pool. Each member is gunzipped by magic
//! (`1f 8b`, despite the `.json` name) and parsed with typed visitors (no `serde_json::Value`
//! per row). Rows inside the bbox and time window, or bracketing a segment that crosses
//! both, are kept with two neighbours on each side so interpolation stays anchored.
//!
//! readsb row: `[dt, lat, lon, alt_baro|"ground"|null, gs_kt, track, flags, vrate_fpm,
//! details|null, source, alt_geom_ft, geom_rate_fpm, ias, roll]` (fields from index 9 only in
//! files from 2022 on). flags: 1 stale, 2 new leg, 4 vrate geometric, 8 altitude geometric.

use crate::DataError;
use fdop_types::engine::{EngineClass, engine_class};
use fdop_types::store::pt_flags;
use fdop_types::{BBox, FPM_TO_MPS, FT_TO_M, KT_TO_MPS};
use rayon::prelude::*;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::Instant;

/// What to keep from a trace archive.
#[derive(Clone, Copy, Debug)]
pub struct IngestSpec {
    /// Spatial filter (caller pads it).
    pub bbox: BBox,
    /// Time filter, unix seconds (caller pads it).
    pub t_start: f64,
    pub t_end: f64,
    /// Geoid undulation N (m): MSL = ellipsoidal height - N.
    pub geoid_n_m: f64,
    /// Keep aircraft whose type designator is missing/unknown (fail-open; default true).
    pub keep_unknown: bool,
    /// Keep known jet aircraft as potential tonal sources (explicit opt-in).
    pub include_jets: bool,
}

/// Nearest reports bracketing each UTC seam, with interpolation/duplicate support.
/// The marker also certifies retained-run and original leg-boundary preservation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct DayContext {
    version: u8,
    pub day_start: f64,
    join_gap_s: f64,
    archive_pad_s: f64,
}

impl DayContext {
    pub fn new(day_start: f64, join_gap_s: f64, archive_pad_s: f64) -> Self {
        Self {
            version: 1,
            day_start,
            join_gap_s,
            archive_pad_s,
        }
    }

    pub fn from_stats(stats: &serde_json::Value) -> Option<Self> {
        let context: Self = serde_json::from_value(stats.get("midnight_context")?.clone()).ok()?;
        (context.version == 1
            && context.day_start.is_finite()
            && context.join_gap_s.is_finite()
            && context.join_gap_s >= 0.0
            && context.archive_pad_s.is_finite()
            && context.archive_pad_s >= 0.0)
            .then_some(context)
    }

    pub fn covers(&self, t0: f64, t1: f64) -> bool {
        t0 <= t1
            && t1 - t0 <= self.join_gap_s
            && [self.day_start, self.day_start + 86_400.0]
                .into_iter()
                .any(|midnight| t0 <= midnight && midnight <= t1)
    }

    pub fn includes(&self, required: Self) -> bool {
        self.day_start == required.day_start
            && self.join_gap_s >= required.join_gap_s
            && self.archive_pad_s >= required.archive_pad_s
    }

    fn retain_support(&self, points: &[RawPoint], keep: &mut [bool]) {
        let mut anchors = Vec::with_capacity(6);
        for midnight in [self.day_start, self.day_start + 86_400.0] {
            let before = points
                .iter()
                .enumerate()
                .filter(|(_, p)| p.t < midnight && midnight - p.t <= self.join_gap_s)
                .max_by(|a, b| a.1.t.total_cmp(&b.1.t));
            let after = points
                .iter()
                .enumerate()
                .filter(|(_, p)| p.t >= midnight && p.t - midnight <= self.join_gap_s)
                .min_by(|a, b| a.1.t.total_cmp(&b.1.t));
            anchors.extend(before.into_iter().chain(after).map(|(_, p)| p.t));
        }
        // True source endpoints can also bridge same-side archive overlap. Keep
        // only their usual interpolation support, not the intervening hours.
        for p in points.first().into_iter().chain(points.last()) {
            if [self.day_start, self.day_start + 86_400.0]
                .into_iter()
                .any(|midnight| (p.t - midnight).abs() <= self.archive_pad_s + self.join_gap_s)
            {
                anchors.push(p.t);
            }
        }
        for (p, kept) in points.iter().zip(keep) {
            // All duplicate candidates must remain available for feed/quality
            // preference and boundary hints; a burst must not introduce a cap.
            *kept |= anchors
                .iter()
                .any(|&t| (p.t - t).abs() < crate::build::DEDUPE_S);
        }
    }
}

/// One retained position. Altitudes are kept separately so the store builder can reconcile
/// geometric and barometric heights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawPoint {
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    /// GNSS height converted to MSL (m), NaN if absent.
    pub alt_geom_msl: f32,
    /// Barometric pressure altitude (m), NaN if absent.
    pub alt_baro_m: f32,
    pub gs_mps: f32,
    pub track_deg: f32,
    pub vrate_mps: f32,
    /// `fdop_types::store::pt_flags` bits.
    pub flags: u8,
    /// A readsb new-leg marker or ground report fell on this point or a dropped row before it.
    pub new_leg: bool,
    /// Original airborne reports were removed before this retained run. Another
    /// feed may bridge it only by retaining uninterrupted overlapping coverage.
    pub retained_break: bool,
    /// Original airborne reports were removed after this retained run.
    pub retained_end: bool,
}

/// One aircraft's retained points from one feed.
#[derive(Clone, Debug)]
pub struct RawAircraft {
    /// Lower-case hex address (non-ICAO addresses keep readsb's `~` prefix).
    pub icao: String,
    pub reg: String,
    /// Upper-case ICAO type designator, "" if unknown.
    pub typ: String,
    pub engine: EngineClass,
    /// Feed label: "prod", "mlat" or "staging".
    pub feed: String,
    pub points: Vec<RawPoint>,
    /// Observed leg boundaries, compacted without changing any split-eligible
    /// gap. Kept separately so source interleaving cannot move an event.
    pub leg_boundaries: crate::build::BoundaryEvents,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IngestStats {
    /// Tar bytes consumed.
    pub bytes: u64,
    /// `trace_full_*` members seen.
    pub members: u64,
    pub parse_errors: u64,
    /// Aircraft with a point or a bracketing airborne segment inside bbox + window.
    pub aircraft_overlap: u64,
    pub jets_dropped: u64,
    pub unknown_kept: u64,
    pub unknown_dropped: u64,
    pub mixed_kept: u64,
    pub kept_aircraft: u64,
    pub points_kept: u64,
}

impl IngestStats {
    pub fn add(&mut self, o: &IngestStats) {
        self.bytes += o.bytes;
        self.members += o.members;
        self.parse_errors += o.parse_errors;
        self.aircraft_overlap += o.aircraft_overlap;
        self.jets_dropped += o.jets_dropped;
        self.unknown_kept += o.unknown_kept;
        self.unknown_dropped += o.unknown_dropped;
        self.mixed_kept += o.mixed_kept;
        self.kept_aircraft += o.kept_aircraft;
        self.points_kept += o.points_kept;
    }
}

/// Periodic progress snapshot.
#[derive(Clone, Debug)]
pub struct IngestProgress {
    pub label: String,
    pub bytes: u64,
    pub members: u64,
    pub kept_aircraft: u64,
    pub elapsed_s: f64,
}

// ------------------------------------------------------------------------------------------
// Typed, lenient JSON visitors
// ------------------------------------------------------------------------------------------

fn drain_seq<'de, A: SeqAccess<'de>>(mut a: A) -> Result<(), A::Error> {
    while a.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut a: A) -> Result<(), A::Error> {
    while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

/// Number or anything else (-> None). Never fails on type mismatch.
#[derive(Clone, Copy, Debug, Default)]
struct LNum(Option<f64>);

impl<'de> Deserialize<'de> for LNum {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LNum;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or null")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<LNum, E> {
                Ok(LNum(v.is_finite().then_some(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<LNum, E> {
                Ok(LNum(Some(v as f64)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<LNum, E> {
                Ok(LNum(Some(v as f64)))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<LNum, E> {
                Ok(LNum(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<LNum, E> {
                Ok(LNum(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<LNum, E> {
                Ok(LNum(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<LNum, E> {
                Ok(LNum(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<LNum, A::Error> {
                drain_seq(a)?;
                Ok(LNum(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<LNum, A::Error> {
                drain_map(a)?;
                Ok(LNum(None))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Alt {
    #[default]
    Null,
    Ground,
    Num(f64),
}

impl<'de> Deserialize<'de> for Alt {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Alt;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an altitude, \"ground\" or null")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Alt, E> {
                Ok(if v.is_finite() {
                    Alt::Num(v)
                } else {
                    Alt::Null
                })
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Alt, E> {
                Ok(Alt::Num(v as f64))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Alt, E> {
                Ok(Alt::Num(v as f64))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Alt, E> {
                Ok(if v == "ground" {
                    Alt::Ground
                } else {
                    Alt::Null
                })
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Alt, E> {
                Ok(Alt::Null)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Alt, E> {
                Ok(Alt::Null)
            }
            fn visit_none<E: de::Error>(self) -> Result<Alt, E> {
                Ok(Alt::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Alt, A::Error> {
                drain_seq(a)?;
                Ok(Alt::Null)
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Alt, A::Error> {
                drain_map(a)?;
                Ok(Alt::Null)
            }
        }
        d.deserialize_any(V)
    }
}

/// Position source -> point flag bits (MLAT / TIS-B), 0 otherwise.
#[derive(Clone, Copy, Debug, Default)]
struct Src(u8);

fn src_bits(s: &str) -> u8 {
    if s.contains("mlat") {
        pt_flags::MLAT
    } else if s.contains("tisb") {
        pt_flags::TISB
    } else {
        0
    }
}

impl<'de> Deserialize<'de> for Src {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Src;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a source string or null")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Src, E> {
                Ok(Src(src_bits(v)))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Src, E> {
                Ok(Src(0))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Src, E> {
                Ok(Src(0))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Src, E> {
                Ok(Src(0))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Src, E> {
                Ok(Src(0))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Src, E> {
                Ok(Src(0))
            }
            fn visit_none<E: de::Error>(self) -> Result<Src, E> {
                Ok(Src(0))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Src, A::Error> {
                drain_seq(a)?;
                Ok(Src(0))
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Src, A::Error> {
                drain_map(a)?;
                Ok(Src(0))
            }
        }
        d.deserialize_any(V)
    }
}

/// Lenient optional string (non-strings -> None).
#[derive(Clone, Debug, Default)]
struct LStr(Option<String>);

impl<'de> Deserialize<'de> for LStr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LStr;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or null")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<LStr, E> {
                Ok(LStr(Some(v.to_string())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<LStr, E> {
                Ok(LStr(Some(v)))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<LStr, E> {
                Ok(LStr(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<LStr, E> {
                Ok(LStr(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<LStr, E> {
                Ok(LStr(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<LStr, E> {
                Ok(LStr(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<LStr, E> {
                Ok(LStr(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<LStr, E> {
                Ok(LStr(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<LStr, A::Error> {
                drain_seq(a)?;
                Ok(LStr(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<LStr, A::Error> {
                drain_map(a)?;
                Ok(LStr(None))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Row {
    dt: Option<f64>,
    lat: Option<f64>,
    lon: Option<f64>,
    alt: Alt,
    gs: Option<f64>,
    trk: Option<f64>,
    flags: i64,
    vrate: Option<f64>,
    src: u8,
    alt_geom: Option<f64>,
    geom_rate: Option<f64>,
}

impl<'de> Deserialize<'de> for Row {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Row;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a readsb trace row array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Row, A::Error> {
                let mut r = Row::default();
                macro_rules! next {
                    ($t:ty) => {
                        match a.next_element::<$t>()? {
                            Some(v) => v,
                            None => return Ok(r),
                        }
                    };
                }
                r.dt = next!(LNum).0;
                r.lat = next!(LNum).0;
                r.lon = next!(LNum).0;
                r.alt = next!(Alt);
                r.gs = next!(LNum).0;
                r.trk = next!(LNum).0;
                r.flags = next!(LNum).0.map_or(0, |f| f as i64);
                r.vrate = next!(LNum).0;
                let _details: IgnoredAny = next!(IgnoredAny);
                r.src = next!(Src).0;
                r.alt_geom = next!(LNum).0;
                r.geom_rate = next!(LNum).0;
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(r)
            }
            // A non-array row (should not happen) is skipped rather than failing the file.
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Row, A::Error> {
                drain_map(a)?;
                Ok(Row::default())
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Row, E> {
                Ok(Row::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Row, E> {
                Ok(Row::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Row, E> {
                Ok(Row::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Row, E> {
                Ok(Row::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Row, E> {
                Ok(Row::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Row, E> {
                Ok(Row::default())
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Default, Deserialize)]
struct TraceFile {
    #[serde(default)]
    icao: LStr,
    #[serde(default)]
    r: LStr,
    #[serde(default)]
    t: LStr,
    #[serde(default)]
    timestamp: LNum,
    #[serde(default)]
    trace: Vec<Row>,
}

// ------------------------------------------------------------------------------------------
// Member parsing
// ------------------------------------------------------------------------------------------

/// Outcome of parsing one member.
enum Parsed {
    Kept(RawAircraft),
    Dropped(DropReason),
    NoOverlap,
    Error,
}

#[derive(Clone, Copy)]
enum DropReason {
    Jet,
    Unknown,
}

fn gunzip_if_needed(b: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, std::io::Error> {
    if b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b {
        let mut out = Vec::with_capacity(b.len() * 6);
        flate2::read::MultiGzDecoder::new(b).read_to_end(&mut out)?;
        Ok(std::borrow::Cow::Owned(out))
    } else {
        Ok(std::borrow::Cow::Borrowed(b))
    }
}

/// Parse one `trace_full` member (gzip or plain JSON) and filter it.
fn parse_member(
    bytes: &[u8],
    name_hint: &str,
    feed: &str,
    spec: &IngestSpec,
    context: Option<DayContext>,
) -> Parsed {
    let Ok(json) = gunzip_if_needed(bytes) else {
        return Parsed::Error;
    };
    let tf: TraceFile = match serde_json::from_slice(&json) {
        Ok(v) => v,
        Err(_) => return Parsed::Error,
    };
    let base = tf.timestamp.0.unwrap_or(0.0);
    let n_geoid = spec.geoid_n_m;
    let mut pts: Vec<RawPoint> = Vec::with_capacity(tf.trace.len());
    let mut leg_boundaries = crate::build::BoundaryEvents::default();
    let mut boundary_overlap = false;
    let mut pending_leg = false;
    let mut untimed_leg = false;
    for row in &tf.trace {
        if row.flags & 2 != 0 || row.alt == Alt::Ground {
            pending_leg = true;
            if let Some(dt) = row.dt {
                leg_boundaries.push(base + dt, row.alt == Alt::Ground);
                boundary_overlap |= context.is_some_and(|c| {
                    (base + dt >= spec.t_start
                        && base + dt <= spec.t_end
                        && row
                            .lat
                            .zip(row.lon)
                            .is_some_and(|(lat, lon)| spec.bbox.contains(lat, lon)))
                        || [c.day_start, c.day_start + 86_400.0]
                            .into_iter()
                            .any(|seam| (base + dt - seam).abs() <= c.join_gap_s)
                });
            } else {
                untimed_leg = true;
            }
            if let Some(previous) = pts.last_mut() {
                previous.flags |= pt_flags::NEW_LEG_AFTER;
            }
        }
        let (Some(dt), Some(lat), Some(lon)) = (row.dt, row.lat, row.lon) else {
            continue;
        };
        if row.alt == Alt::Ground {
            continue;
        }
        let geom = match (row.alt_geom, row.alt) {
            (Some(g), _) => Some(g * FT_TO_M - n_geoid),
            (None, Alt::Num(a)) if row.flags & 8 != 0 => Some(a * FT_TO_M - n_geoid),
            _ => None,
        };
        let baro = match row.alt {
            Alt::Num(a) if row.flags & 8 == 0 => Some(a * FT_TO_M),
            _ => None,
        };
        if geom.is_none() && baro.is_none() {
            continue;
        }
        let mut flags = row.src;
        if row.flags & 1 != 0 {
            flags |= pt_flags::STALE;
        }
        let vrate = row.geom_rate.or(row.vrate);
        if std::mem::take(&mut untimed_leg) {
            leg_boundaries.push(base + dt, false);
        }
        pts.push(RawPoint {
            t: base + dt,
            lat,
            lon,
            alt_geom_msl: geom.map_or(f32::NAN, |v| v as f32),
            alt_baro_m: baro.map_or(f32::NAN, |v| v as f32),
            gs_mps: row.gs.map_or(f32::NAN, |v| (v * KT_TO_MPS) as f32),
            track_deg: row.trk.map_or(f32::NAN, |v| v as f32),
            vrate_mps: vrate.map_or(f32::NAN, |v| (v * FPM_TO_MPS) as f32),
            flags,
            new_leg: std::mem::take(&mut pending_leg),
            retained_break: false,
            retained_end: false,
        });
    }
    if untimed_leg && let Some(previous) = pts.last() {
        leg_boundaries.push(previous.t, true);
    }
    let inside: Vec<bool> = pts
        .iter()
        .map(|p| p.t >= spec.t_start && p.t <= spec.t_end && spec.bbox.contains(p.lat, p.lon))
        .collect();
    let mut keep = inside.clone();
    for (i, pair) in pts.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        if !inside[i]
            && !inside[i + 1]
            && !b.new_leg
            && spec.bbox.intersects_timed_segment(
                (a.t, a.lat, a.lon),
                (b.t, b.lat, b.lon),
                (spec.t_start, spec.t_end),
            )
        {
            keep[i] = true;
            keep[i + 1] = true;
        }
    }
    if let Some(context) = context {
        context.retain_support(&pts, &mut keep);
    }
    if !keep.iter().any(|k| *k) && !boundary_overlap {
        return Parsed::NoOverlap;
    }
    let typ = tf.t.0.as_deref().unwrap_or("").trim().to_ascii_uppercase();
    let engine = engine_class(&typ);
    if engine == EngineClass::Jet && !spec.include_jets {
        return Parsed::Dropped(DropReason::Jet);
    }
    if engine == EngineClass::Unknown && !spec.keep_unknown {
        return Parsed::Dropped(DropReason::Unknown);
    }
    let points = retain_with_neighbours(&pts, &keep, 2);
    leg_boundaries.normalize();
    let icao = tf
        .icao
        .0
        .clone()
        .unwrap_or_else(|| {
            name_hint
                .rsplit('/')
                .next()
                .unwrap_or(name_hint)
                .trim_start_matches("trace_full_")
                .trim_end_matches(".json")
                .to_string()
        })
        .trim()
        .to_ascii_lowercase();
    Parsed::Kept(RawAircraft {
        icao,
        reg: tf.r.0.clone().unwrap_or_default().trim().to_string(),
        typ,
        engine,
        feed: feed.to_string(),
        points,
        leg_boundaries,
    })
}

/// Keep flagged points plus `k` neighbours each side. A new-leg marker on a dropped point is
/// carried to the next retained point. Removed airborne reports end a retained run.
pub(crate) fn retain_with_neighbours(pts: &[RawPoint], keep: &[bool], k: usize) -> Vec<RawPoint> {
    let n = pts.len();
    let mut grown = vec![false; n];
    for (i, kept) in keep.iter().enumerate() {
        if *kept {
            let lo = i.saturating_sub(k);
            let hi = (i + k + 1).min(n);
            for g in &mut grown[lo..hi] {
                *g = true;
            }
        }
    }
    let mut out = Vec::with_capacity(grown.iter().filter(|g| **g).count());
    let mut carry = false;
    let mut dropped = false;
    for (i, (p, g)) in pts.iter().zip(&grown).enumerate() {
        if *g {
            let mut q = *p;
            q.new_leg |= carry;
            q.retained_break |= dropped;
            q.retained_end |= grown.get(i + 1).is_some_and(|kept| !kept);
            carry = false;
            dropped = false;
            out.push(q);
        } else {
            let boundary = p.new_leg || p.flags & pt_flags::NEW_LEG_AFTER != 0;
            carry |= boundary;
            if boundary && let Some(previous) = out.last_mut() {
                previous.flags |= pt_flags::NEW_LEG_AFTER;
            }
            dropped = true;
        }
    }
    out
}

// ------------------------------------------------------------------------------------------
// Tar walking
// ------------------------------------------------------------------------------------------

struct CountingReader<R> {
    inner: R,
    count: Arc<AtomicU64>,
    saw_eof: bool,
}

impl<R: Read> CountingReader<R> {
    /// The tar iterator accepts bare EOF and stops after its first zero block. Require
    /// both end blocks, then drain padding through transport EOF before publishing a day.
    fn finish_tar(mut self) -> Result<(), DataError> {
        if self.saw_eof {
            return Err(DataError("tar is missing the end-of-archive marker".into()));
        }
        let mut block = [0u8; 512];
        self.read_exact(&mut block)
            .map_err(|e| DataError(format!("tar end marker: {e}")))?;
        if block.iter().any(|b| *b != 0) {
            return Err(DataError("tar end marker contains nonzero bytes".into()));
        }
        let mut padding = [0u8; 8192];
        loop {
            match self.read(&mut padding) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    if padding[..n].iter().any(|b| *b != 0) {
                        return Err(DataError(
                            "tar has nonzero data after its end marker".into(),
                        ));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(DataError(format!("tar transport completion: {e}"))),
            }
        }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.saw_eof |= n == 0 && !buf.is_empty();
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

const BATCH_MEMBERS: usize = 1024;
const BATCH_BYTES: usize = 96 << 20;
const PROGRESS_EVERY_S: f64 = 5.0;

fn accumulate(res: Parsed, out: &mut Vec<RawAircraft>, st: &mut IngestStats) {
    match res {
        Parsed::Kept(a) => {
            st.aircraft_overlap += 1;
            match a.engine {
                EngineClass::Unknown => st.unknown_kept += 1,
                EngineClass::Mixed => st.mixed_kept += 1,
                _ => {}
            }
            st.kept_aircraft += 1;
            st.points_kept += a.points.len() as u64;
            out.push(a);
        }
        Parsed::Dropped(DropReason::Jet) => {
            st.aircraft_overlap += 1;
            st.jets_dropped += 1;
        }
        Parsed::Dropped(DropReason::Unknown) => {
            st.aircraft_overlap += 1;
            st.unknown_dropped += 1;
        }
        Parsed::NoOverlap => {}
        Parsed::Error => st.parse_errors += 1,
    }
}

/// Walk a tar stream of `trace_full_*` members and return the retained aircraft.
/// `feed` is the label stored on every aircraft ("prod", "mlat", "staging").
pub fn ingest_reader<R: Read + Send>(
    r: R,
    feed: &str,
    spec: &IngestSpec,
    progress: &(dyn Fn(&IngestProgress) + Sync),
) -> Result<(Vec<RawAircraft>, IngestStats), DataError> {
    ingest_reader_inner(r, feed, spec, progress, None)
}

pub(crate) fn ingest_day_reader<R: Read + Send>(
    r: R,
    feed: &str,
    spec: &IngestSpec,
    progress: &(dyn Fn(&IngestProgress) + Sync),
    context: DayContext,
) -> Result<(Vec<RawAircraft>, IngestStats), DataError> {
    ingest_reader_inner(r, feed, spec, progress, Some(context))
}

fn ingest_reader_inner<R: Read + Send>(
    r: R,
    feed: &str,
    spec: &IngestSpec,
    progress: &(dyn Fn(&IngestProgress) + Sync),
    context: Option<DayContext>,
) -> Result<(Vec<RawAircraft>, IngestStats), DataError> {
    let count = Arc::new(AtomicU64::new(0));
    let reader = CountingReader {
        inner: r,
        count: count.clone(),
        saw_eof: false,
    };
    let (tx, rx) = sync_channel::<Vec<(String, Vec<u8>)>>(2);
    let started = Instant::now();
    let mut out = Vec::new();
    let mut st = IngestStats::default();
    let walk: Result<u64, DataError> = std::thread::scope(|s| {
        let producer = s.spawn(move || -> Result<u64, DataError> {
            let mut archive = tar::Archive::new(reader);
            let mut batch: Vec<(String, Vec<u8>)> = Vec::with_capacity(BATCH_MEMBERS);
            let mut batch_bytes = 0usize;
            let mut members = 0u64;
            let entries = archive
                .entries()
                .map_err(|e| DataError(format!("tar: {e}")))?;
            for entry in entries {
                let mut e = entry.map_err(|e| DataError(format!("tar entry: {e}")))?;
                if !e.header().entry_type().is_file() {
                    continue;
                }
                let name = e
                    .path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !name.contains("trace_full_") {
                    continue;
                }
                let mut buf = Vec::with_capacity(e.size() as usize);
                e.read_to_end(&mut buf)
                    .map_err(|err| DataError(format!("tar read {name}: {err}")))?;
                members += 1;
                batch_bytes += buf.len();
                batch.push((name, buf));
                if batch.len() >= BATCH_MEMBERS || batch_bytes >= BATCH_BYTES {
                    let full = std::mem::replace(&mut batch, Vec::with_capacity(BATCH_MEMBERS));
                    batch_bytes = 0;
                    if tx.send(full).is_err() {
                        return Ok(members);
                    }
                }
            }
            archive.into_inner().finish_tar()?;
            if !batch.is_empty() {
                let _ = tx.send(batch);
            }
            Ok(members)
        });
        let mut last = Instant::now();
        for batch in rx {
            let results: Vec<Parsed> = batch
                .par_iter()
                .map(|(name, b)| parse_member(b, name, feed, spec, context))
                .collect();
            st.members += batch.len() as u64;
            for res in results {
                accumulate(res, &mut out, &mut st);
            }
            if last.elapsed().as_secs_f64() >= PROGRESS_EVERY_S {
                last = Instant::now();
                progress(&IngestProgress {
                    label: feed.to_string(),
                    bytes: count.load(Ordering::Relaxed),
                    members: st.members,
                    kept_aircraft: st.kept_aircraft,
                    elapsed_s: started.elapsed().as_secs_f64(),
                });
            }
        }
        producer
            .join()
            .map_err(|_| DataError("tar reader thread panicked".into()))?
    });
    walk?;
    st.bytes = count.load(Ordering::Relaxed);
    progress(&IngestProgress {
        label: feed.to_string(),
        bytes: st.bytes,
        members: st.members,
        kept_aircraft: st.kept_aircraft,
        elapsed_s: started.elapsed().as_secs_f64(),
    });
    Ok((out, st))
}

/// Ingest a local source: a `.tar` (or concatenated split parts given as a directory of
/// `*.tar.a?` files is NOT supported; concatenate first), a directory tree of
/// `trace_full_*` files, or a single `trace_full_*` file.
pub fn ingest_path(
    path: &Path,
    feed: &str,
    spec: &IngestSpec,
    progress: &(dyn Fn(&IngestProgress) + Sync),
) -> Result<(Vec<RawAircraft>, IngestStats), DataError> {
    if path.is_dir() {
        let mut files = Vec::new();
        collect_trace_files(path, &mut files)?;
        files.sort();
        let mut out = Vec::new();
        let mut st = IngestStats::default();
        for chunk in files.chunks(BATCH_MEMBERS) {
            let results: Vec<(Parsed, u64)> = chunk
                .par_iter()
                .map(|p| match std::fs::read(p) {
                    Ok(b) => (
                        parse_member(&b, &p.to_string_lossy(), feed, spec, None),
                        b.len() as u64,
                    ),
                    Err(_) => (Parsed::Error, 0),
                })
                .collect();
            for (res, nb) in results {
                st.members += 1;
                st.bytes += nb;
                accumulate(res, &mut out, &mut st);
            }
        }
        progress(&IngestProgress {
            label: feed.to_string(),
            bytes: st.bytes,
            members: st.members,
            kept_aircraft: st.kept_aircraft,
            elapsed_s: 0.0,
        });
        return Ok((out, st));
    }
    let mut f =
        std::fs::File::open(path).map_err(|e| DataError(format!("{}: {e}", path.display())))?;
    let mut head = [0u8; 512];
    let n = f.read(&mut head)?;
    let is_tar = n >= 262 && &head[257..262] == b"ustar";
    drop(f);
    if is_tar {
        let f = std::fs::File::open(path)?;
        ingest_reader(
            std::io::BufReader::with_capacity(1 << 20, f),
            feed,
            spec,
            progress,
        )
    } else {
        let b = std::fs::read(path)?;
        let mut out = Vec::new();
        let mut st = IngestStats {
            members: 1,
            bytes: b.len() as u64,
            ..Default::default()
        };
        accumulate(
            parse_member(&b, &path.to_string_lossy(), feed, spec, None),
            &mut out,
            &mut st,
        );
        Ok((out, st))
    }
}

fn collect_trace_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), DataError> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            collect_trace_files(&p, out)?;
        } else if p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("trace_full_"))
        {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    pub(crate) const T0: f64 = 1_788_825_600.0; // 2026-09-08T00:00:00Z

    pub(crate) fn spec() -> IngestSpec {
        IngestSpec {
            bbox: BBox::new(40.0, -74.0, 41.0, -73.0),
            t_start: T0 + 70_000.0,
            t_end: T0 + 80_000.0,
            geoid_n_m: -32.0,
            keep_unknown: true,
            include_jets: false,
        }
    }

    pub(crate) fn gz(s: &str) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::fast());
        e.write_all(s.as_bytes()).unwrap();
        e.finish().unwrap()
    }

    pub(crate) fn tar_of(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in members {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            b.append_data(&mut h, name, data.as_slice()).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn tar_requires_both_end_blocks_after_the_last_member() {
        let bytes = tar_of(&[("trace_full_a1b2c3.json", gz(&pc12_json()))]);
        let payload_end = bytes.len() - 1024;
        for tail in [0, 1, 511, 512, 513, 1023] {
            let result = ingest_reader(&bytes[..payload_end + tail], "prod", &spec(), &|_| {});
            assert!(result.is_err(), "accepted only {tail} end-marker bytes");
        }
        let mut corrupt = bytes;
        let second_block = corrupt.len() - 512;
        corrupt[second_block] = 1;
        assert!(ingest_reader(corrupt.as_slice(), "prod", &spec(), &|_| {}).is_err());
    }

    #[test]
    fn tar_checks_transport_completion_after_the_end_marker() {
        struct FailsAtEof<R>(R);
        impl<R: Read> Read for FailsAtEof<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.read(buf)? {
                    0 if !buf.is_empty() => Err(std::io::Error::other("transport incomplete")),
                    n => Ok(n),
                }
            }
        }
        let bytes = tar_of(&[("trace_full_a1b2c3.json", gz(&pc12_json()))]);
        let error = ingest_reader(FailsAtEof(bytes.as_slice()), "prod", &spec(), &|_| {})
            .expect_err("tar end markers cannot prove the HTTP response completed");
        assert!(error.to_string().contains("transport incomplete"));
    }

    #[test]
    fn tar_drains_zero_padding_and_keeps_split_member_boundaries() {
        struct Chunks<R>(R, usize);
        impl<R: Read> Read for Chunks<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(self.1);
                self.0.read(&mut buf[..n])
            }
        }
        let mut bytes = tar_of(&[("trace_full_a1b2c3.json", gz(&pc12_json()))]);
        bytes.resize(bytes.len() + 9 * 512, 0); // normal tar blocking-factor padding
        for split in [1, 17, 511, 512, 1025] {
            let (head, tail) = bytes.split_at(split);
            let reader = head.chain(tail);
            let (aircraft, stats) =
                ingest_reader(Chunks(reader, 37), "prod", &spec(), &|_| {}).unwrap();
            assert_eq!(aircraft.len(), 1);
            assert_eq!(aircraft[0].icao, "a1b2c3");
            assert_eq!(stats.members, 1);
            assert_eq!(stats.bytes, bytes.len() as u64);
        }
        let empty = tar_of(&[]);
        let (aircraft, stats) = ingest_reader(empty.as_slice(), "prod", &spec(), &|_| {}).unwrap();
        assert!(aircraft.is_empty());
        assert_eq!(stats.bytes, empty.len() as u64);
    }

    #[test]
    fn tar_rejects_nonzero_data_after_the_end_marker() {
        let mut bytes = tar_of(&[("trace_full_a1b2c3.json", gz(&pc12_json()))]);
        bytes.extend_from_slice(b"unexpected second archive or damaged padding");
        assert!(ingest_reader(bytes.as_slice(), "prod", &spec(), &|_| {}).is_err());
    }

    /// PC-12: 10 rows, rows 0-1 outside bbox (west), row 2 "ground", rows 3-9 inside, a
    /// new-leg flag on row 5. Geom on rows 3,4,6 (plus baro), baro-only on 7,8,9.
    pub(crate) fn pc12_json() -> String {
        let rows = [
            r#"[70000.0, 40.5, -74.5, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70010.0, 40.5, -74.2, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70020.0, 40.5, -74.1, "ground", 0, 90, 0, null, null, "adsb_icao", null, null, null, null]"#,
            r#"[70030.0, 40.5, -73.9, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70040.0, 40.5, -73.8, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70050.0, 40.5, -73.7, 5000, 200, 90, 2, 0, null, "adsb_icao", null, null, null, null]"#,
            r#"[70060.0, 40.5, -73.6, 5000, 200, 90, 0, -500, {"x":1}, "adsb_icao", 5100, -640, null, null]"#,
            r#"[70070.0, 40.5, -73.5, 5000, 200, 90, 0, 0, null, "adsb_icao", null, null, null, null]"#,
            r#"[70080.0, 40.5, -73.4, 5000, 200, 90, 1, 0, null, "mlat", null, null, null, null]"#,
            r#"[70090.0, 40.5, -73.3, 5000, 200, 90, 0, 0, null, "adsb_icao", null, null, null, null]"#,
        ];
        format!(
            r#"{{"icao":"A1B2C3","r":"N123PC","t":"PC12","dbFlags":0,"timestamp":{T0},"trace":[{}]}}"#,
            rows.join(",")
        )
    }

    #[test]
    fn parses_filters_and_classifies() {
        let jet = format!(
            r#"{{"icao":"abcdef","t":"B738","timestamp":{T0},"trace":[[70000.0,40.5,-73.5,30000,400,90,0,0,null,"adsb_icao",30200,0,null,null]]}}"#
        );
        let unk = format!(
            r#"{{"icao":"123456","timestamp":{T0},"trace":[[71000.0,40.6,-73.5,3000,100,90,8,0,null,"adsb_icao",null,null,null,null]]}}"#
        );
        let far = format!(
            r#"{{"icao":"654321","t":"C172","timestamp":{T0},"trace":[[71000.0,35.0,-100.0,3000,100,90,0,0,null,"adsb_icao",null,null,null,null]]}}"#
        );
        let old_short_rows = format!(
            r#"{{"icao":"777777","t":"BE20","timestamp":{T0},"trace":[[72000.0,40.7,-73.2,4000,180,10,0,null,null],[72010.0,40.71,-73.2,4000,180,10,0,null,null]]}}"#
        );
        let tarb = tar_of(&[
            ("./traces/c3/trace_full_a1b2c3.json", gz(&pc12_json())),
            ("./traces/ef/trace_full_abcdef.json", gz(&jet)),
            ("./traces/56/trace_full_123456.json", unk.into_bytes()),
            ("./traces/21/trace_full_654321.json", gz(&far)),
            ("./traces/77/trace_full_777777.json", gz(&old_short_rows)),
            ("./traces/zz/trace_full_broken.json", b"{not json".to_vec()),
            ("./acas/acas.csv", b"x".to_vec()),
        ]);
        let (air, st) =
            ingest_reader(std::io::Cursor::new(tarb), "prod", &spec(), &|_| {}).unwrap();
        assert_eq!(st.members, 6);
        assert_eq!(st.parse_errors, 1);
        assert_eq!(st.jets_dropped, 1);
        assert_eq!(st.unknown_kept, 1);
        assert_eq!(st.kept_aircraft, 3);
        let pc = air.iter().find(|a| a.icao == "a1b2c3").unwrap();
        assert_eq!(pc.typ, "PC12");
        assert_eq!(pc.engine, EngineClass::Turboprop);
        // inside: rows 3..9 (7 rows); neighbours: rows 0,1 (the ground row is not a point)
        assert_eq!(pc.points.len(), 9);
        assert_eq!(pc.points[0].t, T0 + 70_000.0);
        // geom 5100 ft ellipsoidal -> MSL = 1554.48 + 32
        assert!((pc.points[2].alt_geom_msl - (5100.0 * 0.3048 + 32.0) as f32).abs() < 0.01);
        assert!((pc.points[2].alt_baro_m - 1524.0).abs() < 0.01);
        assert!(pc.points[4].alt_geom_msl.is_nan() && pc.points[4].new_leg);
        assert!(!pc.points[3].new_leg);
        assert!((pc.points[5].vrate_mps - (-640.0 * 0.3048 / 60.0) as f32).abs() < 1e-4);
        assert_eq!(pc.points[7].flags & pt_flags::MLAT, pt_flags::MLAT);
        assert_eq!(pc.points[7].flags & pt_flags::STALE, pt_flags::STALE);
        assert!((pc.points[0].gs_mps - (200.0 * 0.514444) as f32).abs() < 1e-3);
        let unk = air.iter().find(|a| a.icao == "123456").unwrap();
        // flags&8: alt field is geometric
        assert!((unk.points[0].alt_geom_msl - (3000.0 * 0.3048 + 32.0) as f32).abs() < 0.01);
        assert!(unk.points[0].alt_baro_m.is_nan());
        let old = air.iter().find(|a| a.icao == "777777").unwrap();
        assert_eq!(old.points.len(), 2);
        assert!(
            old.points[0].alt_geom_msl.is_nan() && (old.points[0].alt_baro_m - 1219.2).abs() < 0.01
        );
    }

    #[test]
    fn malformed_scalar_rows_preserve_usable_trace() {
        let sp = IngestSpec {
            bbox: BBox::new(0.0, 0.0, 0.1, 0.1),
            t_start: T0,
            t_end: T0 + 300.0,
            ..spec()
        };
        let point = |t, lon| {
            serde_json::json!([
                t,
                0.05,
                lon,
                4000,
                100,
                90,
                0,
                0,
                null,
                "adsb_icao",
                4100,
                0
            ])
        };
        let rows = vec![
            point(0, 0.02),
            point(10, 0.03),
            serde_json::json!([30, null, null, "ground"]),
            point(60, 0.04),
            point(70, 0.05),
        ];
        let read = |rows: &[serde_json::Value]| {
            let json =
                serde_json::json!({"icao":"000001", "t":"PC12", "timestamp":T0, "trace":rows});
            let tar = tar_of(&[("trace_full_000001.json", gz(&json.to_string()))]);
            ingest_reader(tar.as_slice(), "prod", &sp, &|_| {}).unwrap()
        };
        let expected = read(&rows);
        assert_eq!(expected.0[0].points.len(), 4);
        let expected_store = crate::build::build_store(vec![expected.clone()], &sp, vec![]);
        assert_eq!(expected_store.legs.len(), 2);
        for malformed in [
            serde_json::json!(true),
            serde_json::json!(-1),
            serde_json::json!(u64::MAX),
            serde_json::json!(1.5),
            serde_json::json!("bad row"),
            serde_json::Value::Null,
            serde_json::json!({"bad": [true, null]}),
        ] {
            let mut mixed = vec![malformed.clone()];
            for row in &rows {
                mixed.push(row.clone());
                mixed.push(malformed.clone());
            }
            let actual = read(&mixed);
            assert_eq!(actual.1.parse_errors, 0, "row={malformed}");
            assert_eq!(actual.1.kept_aircraft, 1, "row={malformed}");
            assert_eq!(actual.0[0].points, expected.0[0].points);
            let store = crate::build::build_store(vec![actual], &sp, vec![]);
            assert_eq!(
                store.legs.len(),
                2,
                "ground boundary lost for row={malformed}"
            );
            for (leg, expected_leg) in store.legs.iter().zip(&expected_store.legs) {
                assert_eq!(leg.points, expected_leg.points);
            }
        }
    }

    #[test]
    fn unknown_can_be_dropped() {
        let unk = format!(
            r#"{{"icao":"123456","timestamp":{T0},"trace":[[71000.0,40.6,-73.5,3000,100,90,0,0,null,"adsb_icao",null,null,null,null]]}}"#
        );
        let tarb = tar_of(&[("./traces/56/trace_full_123456.json", gz(&unk))]);
        let mut sp = spec();
        sp.keep_unknown = false;
        let (air, st) = ingest_reader(std::io::Cursor::new(tarb), "prod", &sp, &|_| {}).unwrap();
        assert!(air.is_empty());
        assert_eq!(st.unknown_dropped, 1);
    }

    #[test]
    fn jets_are_retained_only_when_explicitly_requested() {
        let jet = pc12_json().replace("PC12", "b738");
        let bytes = tar_of(&[("trace_full_a1b2c3.json", gz(&jet))]);
        for include_jets in [false, true] {
            for keep_unknown in [false, true] {
                let sp = IngestSpec {
                    include_jets,
                    keep_unknown,
                    ..spec()
                };
                let (aircraft, stats) =
                    ingest_reader(bytes.as_slice(), "prod", &sp, &|_| {}).unwrap();
                assert_eq!(stats.parse_errors, 0);
                assert_eq!(stats.jets_dropped, u64::from(!include_jets));
                assert_eq!(aircraft.len(), usize::from(include_jets));
                if include_jets {
                    assert_eq!(aircraft[0].engine, EngineClass::Jet);
                    assert_eq!(aircraft[0].typ, "B738");
                    assert_eq!(aircraft[0].points.len(), 9);
                    let store = crate::build::build_store(vec![(aircraft, stats)], &sp, vec![]);
                    assert_eq!(store.legs.len(), 1);
                    assert_eq!(store.legs[0].points.len(), 9);
                    assert!(store.legs.iter().all(|leg| leg.engine == EngineClass::Jet));
                    assert_eq!(store.meta.stats["include_jets"], true);
                }
            }
        }
    }

    #[test]
    fn ingest_and_cache_keep_sparse_crossing_segments() {
        let (lat, lon) = (40.876466, -73.524951);
        let rows: Vec<_> = [(0.0, -0.09), (30.0, -0.06), (150.0, 0.06), (180.0, 0.09)]
            .into_iter()
            .map(|(t, dx)| {
                serde_json::json!([
                    t,
                    lat,
                    lon + dx,
                    4000,
                    165,
                    90,
                    0,
                    0,
                    null,
                    "adsb_icao",
                    4100,
                    0
                ])
            })
            .collect();
        let make = |rows: &[serde_json::Value]| {
            let json =
                serde_json::json!({"icao":"000001", "t":"C172", "timestamp":T0, "trace":rows});
            tar_of(&[("trace_full_000001.json", gz(&json.to_string()))])
        };
        let sp = IngestSpec {
            bbox: BBox::new(lat - 0.002, lon - 0.002, lat + 0.002, lon + 0.002),
            t_start: T0,
            t_end: T0 + 300.0,
            ..spec()
        };
        let read = |rows: &[serde_json::Value], sp: &IngestSpec| {
            ingest_reader(std::io::Cursor::new(make(rows)), "prod", sp, &|_| {}).unwrap()
        };
        let (aircraft, stats) = read(&rows, &sp);
        assert_eq!(stats.kept_aircraft, 1, "no reported knot is inside the box");
        assert_eq!(aircraft[0].points.len(), 4);
        let store =
            crate::build::build_store(vec![(aircraft, stats)], &sp, vec!["synthetic".into()]);
        assert_eq!(store.legs.len(), 1);
        let dir = std::env::temp_dir().join(format!(
            "fdop-crossing-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("day.fdtrk");
        store.save(&path).unwrap();
        let cached = fdop_types::store::TrackStore::load(&path).unwrap();
        let cropped = cached.filtered(&sp.bbox, T0 + 89.0, T0 + 91.0);
        assert_eq!(cropped.legs.len(), 1, "cache slicing lost the crossing");
        assert_eq!(cropped.legs[0].points, store.legs[0].points);
        assert!(
            cached
                .filtered(&sp.bbox, T0 + 35.0, T0 + 45.0)
                .legs
                .is_empty()
        );
        let missed_time = IngestSpec {
            t_start: T0 + 35.0,
            t_end: T0 + 45.0,
            ..sp
        };
        assert!(read(&rows, &missed_time).0.is_empty());

        // A restart or an intervening ground report is not an airborne crossing.
        let mut restart = rows.clone();
        restart[2][6] = serde_json::json!(2);
        assert!(read(&restart, &sp).0.is_empty());
        let mut grounded = rows.clone();
        grounded.insert(2, serde_json::json!([90, lat, lon, "ground", 0, 90, 0, 0]));
        assert!(read(&grounded, &sp).0.is_empty());
        grounded[2][1] = serde_json::Value::Null;
        assert!(read(&grounded, &sp).0.is_empty());
        let mut reversed = rows.clone();
        reversed.reverse();
        assert!(read(&reversed, &sp).0.is_empty());
    }

    #[test]
    fn midnight_support_is_bounded_and_keeps_duplicate_candidates() {
        let point = |t| RawPoint {
            t,
            lat: 1.0,
            lon: 1.0,
            alt_geom_msl: 1000.0,
            alt_baro_m: 1000.0,
            gs_mps: 100.0,
            track_deg: 90.0,
            vrate_mps: 0.0,
            flags: 0,
            new_leg: false,
            retained_break: false,
            retained_end: false,
        };
        let context = DayContext::new(T0, 300.0, 3600.0);
        let points: Vec<_> = (0..9000).map(|i| point(T0 + i as f64 * 10.0)).collect();
        let mut keep = vec![false; points.len()];
        context.retain_support(&points, &mut keep);
        let retained = retain_with_neighbours(&points, &keep, 2);
        assert!(
            retained.len() <= 18,
            "chronological support must not retain worldwide hours"
        );
        assert!(retained.iter().any(|p| p.t == T0 + 86_390.0));
        assert!(retained.iter().any(|p| p.t == T0 + 86_400.0));
        let burst: Vec<_> = (0..100)
            .map(|i| point(T0 + 86_400.0 + i as f64 * 0.0004))
            .collect();
        let mut keep = vec![false; burst.len()];
        context.retain_support(&burst, &mut keep);
        assert!(
            keep.into_iter().all(|kept| kept),
            "duplicate preference must see the whole burst"
        );
    }

    #[test]
    fn new_leg_carried_from_dropped_row() {
        let p = |t: f64, leg: bool| RawPoint {
            t,
            lat: 0.0,
            lon: 0.0,
            alt_geom_msl: 0.0,
            alt_baro_m: f32::NAN,
            gs_mps: 0.0,
            track_deg: 0.0,
            vrate_mps: 0.0,
            flags: 0,
            new_leg: leg,
            retained_break: false,
            retained_end: false,
        };
        let pts: Vec<RawPoint> = (0..10).map(|i| p(i as f64, i == 3)).collect();
        let keep: Vec<bool> = (0..10).map(|i| i == 0 || i == 9).collect();
        let out = retain_with_neighbours(&pts, &keep, 2);
        let ts: Vec<f64> = out.iter().map(|q| q.t).collect();
        assert_eq!(ts, vec![0.0, 1.0, 2.0, 7.0, 8.0, 9.0]);
        assert!(out[3].new_leg && !out[0].new_leg && !out[2].new_leg);
    }
}
