//! Turn raw per-feed aircraft into a [`TrackStore`]: merge feeds and days per ICAO address,
//! dedupe, split legs, and reconcile geometric vs barometric heights.
//!
//! Altitude policy (reviewer item 6: never mix geom and baro within a leg uncorrected):
//! * points with a GNSS height use it (MSL via the geoid) -> `GEOM`;
//! * baro-only points get `baro + offset`, where offset = geom_msl - baro measured on the
//!   same leg: median of offsets within +-10 min, else the nearest one in time
//!   -> `BARO_CORRECTED`;
//! * legs without any GNSS height use the regional median offset (aircraft within the
//!   surrounding 3x3 one-degree cells and +-30 min) -> `BARO_CORRECTED`, else raw baro
//!   -> `BARO_RAW`.

use crate::adsblol::Feed;
use crate::ingest::{IngestSpec, IngestStats, RawAircraft, RawPoint};
use fdop_types::engine::{EngineClass, engine_class};
use fdop_types::store::{FlightLeg, StoreMeta, TrackPoint, TrackStore, alt_src, pt_flags};
use fdop_types::time::utc_date;
use std::collections::{BTreeMap, HashMap};

/// Gap that always starts a new leg (s).
pub const LEG_GAP_S: f64 = 1800.0;
/// A readsb new-leg marker is honoured only across a gap of at least this (s): markers from
/// a second feed's leg detector must not split a continuous merged track.
pub const NEW_LEG_MIN_GAP_S: f64 = 30.0;
/// Points closer than this (s) are duplicates.
pub const DEDUPE_S: f64 = 0.05;
const LEG_OFFSET_WINDOW_S: f64 = 600.0;
const REGION_OFFSET_WINDOW_S: f64 = 1800.0;
const STORE_VERSION: u32 = 1;

#[derive(Clone, Copy)]
struct MPoint {
    p: RawPoint,
    rank: u8,
    feed_bit: u8,
}

fn feed_of(label: &str) -> Feed {
    Feed::from_label(label).unwrap_or(Feed::Prod)
}

fn feed_bit(f: Feed) -> u8 {
    match f {
        Feed::Prod => 1,
        Feed::Mlatonly => 2,
        Feed::Staging => 4,
    }
}

fn source_label(bits: u8) -> String {
    let mut v = Vec::new();
    if bits & 1 != 0 {
        v.push("prod");
    }
    if bits & 4 != 0 {
        v.push("staging");
    }
    if bits & 2 != 0 {
        v.push("mlat");
    }
    v.join("+")
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    Some(*m)
}

/// Spatio-temporal index of measured geom-baro offsets for the regional fallback.
struct OffsetIndex {
    cells: HashMap<(i32, i32), Vec<(f64, f32)>>,
    memo: std::sync::Mutex<HashMap<(i32, i32, i64), Option<f32>>>,
}

impl OffsetIndex {
    fn new(samples: Vec<(f64, f64, f64, f32)>) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<(f64, f32)>> = HashMap::new();
        for (t, lat, lon, off) in samples {
            cells
                .entry((lat.floor() as i32, lon.floor() as i32))
                .or_default()
                .push((t, off));
        }
        for v in cells.values_mut() {
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        Self {
            cells,
            memo: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn query(&self, t: f64, lat: f64, lon: f64) -> Option<f32> {
        let (ci, cj, tb) = (
            lat.floor() as i32,
            lon.floor() as i32,
            (t / 300.0).floor() as i64,
        );
        if let Some(v) = self
            .memo
            .lock()
            .ok()
            .and_then(|m| m.get(&(ci, cj, tb)).copied())
        {
            return v;
        }
        let tc = (tb as f64 + 0.5) * 300.0;
        let mut vals: Vec<f32> = Vec::new();
        for di in -1..=1 {
            for dj in -1..=1 {
                if let Some(v) = self.cells.get(&(ci + di, cj + dj)) {
                    let lo = v.partition_point(|x| x.0 < tc - REGION_OFFSET_WINDOW_S);
                    let hi = v.partition_point(|x| x.0 <= tc + REGION_OFFSET_WINDOW_S);
                    let slice = &v[lo..hi];
                    let step = (slice.len() / 400).max(1);
                    vals.extend(slice.iter().step_by(step).map(|x| x.1));
                }
            }
        }
        let r = median(&mut vals);
        if let Ok(mut m) = self.memo.lock() {
            m.insert((ci, cj, tb), r);
        }
        r
    }
}

struct LegDraft {
    icao: String,
    reg: String,
    typ: String,
    engine: EngineClass,
    feeds: u8,
    pts: Vec<MPoint>,
}

/// Merge raw aircraft from any number of ingests (feeds, days) into a store.
pub fn build_store(
    parts: Vec<(Vec<RawAircraft>, IngestStats)>,
    spec: &IngestSpec,
    sources: Vec<String>,
) -> TrackStore {
    let mut stats = IngestStats::default();
    let mut by_icao: BTreeMap<String, Vec<RawAircraft>> = BTreeMap::new();
    for (air, st) in parts {
        stats.add(&st);
        for a in air {
            by_icao.entry(a.icao.clone()).or_default().push(a);
        }
    }
    let mut merges = 0u64;
    let mut dedup_dropped = 0u64;
    let mut jets_late = 0u64;
    let mut drafts: Vec<LegDraft> = Vec::new();
    for (icao, mut group) in by_icao {
        if group.len() > 1 {
            merges += 1;
        }
        group.sort_by_key(|a| feed_of(&a.feed).rank());
        let typ = group
            .iter()
            .map(|a| a.typ.as_str())
            .find(|t| !t.is_empty())
            .unwrap_or("")
            .to_string();
        let reg = group
            .iter()
            .map(|a| a.reg.as_str())
            .find(|t| !t.is_empty())
            .unwrap_or("")
            .to_string();
        let engine = if typ.is_empty() {
            EngineClass::Unknown
        } else {
            engine_class(&typ)
        };
        if engine == EngineClass::Jet {
            jets_late += 1;
            continue;
        }
        if engine == EngineClass::Unknown && !spec.keep_unknown {
            continue;
        }
        let mut pts: Vec<MPoint> = Vec::new();
        for a in &group {
            let f = feed_of(&a.feed);
            pts.extend(a.points.iter().map(|p| MPoint {
                p: *p,
                rank: f.rank(),
                feed_bit: feed_bit(f),
            }));
        }
        pts.sort_by(|a, b| a.p.t.total_cmp(&b.p.t).then(a.rank.cmp(&b.rank)));
        let mut dd: Vec<MPoint> = Vec::with_capacity(pts.len());
        for q in pts {
            if let Some(last) = dd.last_mut()
                && (q.p.t - last.p.t).abs() < DEDUPE_S
            {
                dedup_dropped += 1;
                if q.rank < last.rank {
                    let keep_leg = last.p.new_leg;
                    *last = q;
                    last.p.new_leg |= keep_leg;
                } else {
                    last.p.new_leg |= q.p.new_leg;
                }
                continue;
            }
            dd.push(q);
        }
        // split into legs
        let mut cur: Vec<MPoint> = Vec::new();
        let flush = |cur: &mut Vec<MPoint>, drafts: &mut Vec<LegDraft>| {
            if cur.len() >= 2 {
                let feeds = cur.iter().fold(0u8, |b, q| b | q.feed_bit);
                drafts.push(LegDraft {
                    icao: icao.clone(),
                    reg: reg.clone(),
                    typ: typ.clone(),
                    engine,
                    feeds,
                    pts: std::mem::take(cur),
                });
            } else {
                cur.clear();
            }
        };
        for q in dd {
            if let Some(last) = cur.last() {
                let gap = q.p.t - last.p.t;
                if gap > LEG_GAP_S || (q.p.new_leg && gap >= NEW_LEG_MIN_GAP_S) {
                    flush(&mut cur, &mut drafts);
                }
            }
            cur.push(q);
        }
        flush(&mut cur, &mut drafts);
    }

    // regional offset samples from every leg
    let samples: Vec<(f64, f64, f64, f32)> = drafts
        .iter()
        .flat_map(|d| d.pts.iter())
        .filter(|q| q.p.alt_geom_msl.is_finite() && q.p.alt_baro_m.is_finite())
        .map(|q| (q.p.t, q.p.lat, q.p.lon, q.p.alt_geom_msl - q.p.alt_baro_m))
        .collect();
    let region = OffsetIndex::new(samples);

    let (mut n_geom, mut n_corr, mut n_raw) = (0u64, 0u64, 0u64);
    let mut legs: Vec<FlightLeg> = Vec::with_capacity(drafts.len());
    let mut leg_no: HashMap<String, u32> = HashMap::new();
    let mut seen_keys: HashMap<String, u32> = HashMap::new();
    let mut key_collisions = 0u64;
    for d in drafts {
        let offs: Vec<(f64, f32)> = d
            .pts
            .iter()
            .filter(|q| q.p.alt_geom_msl.is_finite() && q.p.alt_baro_m.is_finite())
            .map(|q| (q.p.t, q.p.alt_geom_msl - q.p.alt_baro_m))
            .collect();
        let mut points = Vec::with_capacity(d.pts.len());
        for q in &d.pts {
            let p = &q.p;
            let (alt, src) = if p.alt_geom_msl.is_finite() {
                n_geom += 1;
                (p.alt_geom_msl, alt_src::GEOM)
            } else {
                let leg_off = if offs.is_empty() {
                    None
                } else {
                    let lo = offs.partition_point(|x| x.0 < p.t - LEG_OFFSET_WINDOW_S);
                    let hi = offs.partition_point(|x| x.0 <= p.t + LEG_OFFSET_WINDOW_S);
                    if hi > lo {
                        let mut v: Vec<f32> = offs[lo..hi].iter().map(|x| x.1).collect();
                        median(&mut v)
                    } else {
                        offs.iter()
                            .min_by(|a, b| (a.0 - p.t).abs().total_cmp(&(b.0 - p.t).abs()))
                            .map(|x| x.1)
                    }
                };
                match leg_off.or_else(|| region.query(p.t, p.lat, p.lon)) {
                    Some(o) => {
                        n_corr += 1;
                        (p.alt_baro_m + o, alt_src::BARO_CORRECTED)
                    }
                    None => {
                        n_raw += 1;
                        (p.alt_baro_m, alt_src::BARO_RAW)
                    }
                }
            };
            points.push(TrackPoint {
                t: p.t,
                lat: p.lat,
                lon: p.lon,
                alt_m: alt,
                gs_mps: p.gs_mps,
                track_deg: p.track_deg,
                vrate_mps: p.vrate_mps,
                alt_src: src,
                flags: p.flags & (pt_flags::MLAT | pt_flags::TISB | pt_flags::STALE),
            });
        }
        let n = leg_no.entry(d.icao.clone()).or_insert(0);
        let (y, m, dd) = utc_date(points[0].t);
        let mut key = format!("{}_{y:04}{m:02}{dd:02}_{n}", d.icao);
        *n += 1;
        let c = seen_keys.entry(key.clone()).or_insert(0);
        if *c > 0 {
            key_collisions += 1;
            key = format!("{key}~{c}");
        }
        *c += 1;
        legs.push(FlightLeg {
            key,
            icao: d.icao,
            reg: d.reg,
            typ: d.typ,
            source: source_label(d.feeds),
            engine: d.engine,
            points,
        });
    }
    legs.sort_by(|a, b| a.t0().total_cmp(&b.t0()).then_with(|| a.key.cmp(&b.key)));
    let n_points: u64 = legs.iter().map(|l| l.points.len() as u64).sum();
    let meta = StoreMeta {
        version: STORE_VERSION,
        bbox: spec.bbox,
        t_start: spec.t_start,
        t_end: spec.t_end,
        geoid_n_m: spec.geoid_n_m,
        sources,
        stats: serde_json::json!({
            "files": stats.members,
            "bytes": stats.bytes,
            "parse_errors": stats.parse_errors,
            "aircraft_overlap": stats.aircraft_overlap,
            "jets_dropped": stats.jets_dropped + jets_late,
            "unknown_kept": stats.unknown_kept,
            "unknown_dropped": stats.unknown_dropped,
            "mixed_kept": stats.mixed_kept,
            "aircraft_kept": stats.kept_aircraft,
            "legs": legs.len(),
            "points": n_points,
            "alt_geom": n_geom,
            "baro_corrected": n_corr,
            "baro_raw": n_raw,
            "feed_merges": merges,
            "dedup_dropped": dedup_dropped,
            "key_collisions": key_collisions,
        }),
        created_unix: crate::now_unix(),
    };
    TrackStore { meta, legs }
}

/// Merge stores (e.g. consecutive UTC days). Legs of the same aircraft from DIFFERENT stores
/// are joined when they overlap in time or the gap is <= `join_gap_s` (a flight crossing
/// midnight UTC); legs from the same store are never joined. Duplicate instants (< 0.05 s)
/// keep the non-MLAT point. Keys are re-assigned `{icao}_{YYYYMMDD}_{n}`.
pub fn merge_stores(stores: Vec<TrackStore>, join_gap_s: f64) -> TrackStore {
    let mut meta = match stores.first() {
        Some(s) => s.meta.clone(),
        None => {
            return TrackStore {
                meta: StoreMeta {
                    version: STORE_VERSION,
                    bbox: fdop_types::BBox::new(0.0, 0.0, 0.0, 0.0),
                    t_start: 0.0,
                    t_end: 0.0,
                    geoid_n_m: 0.0,
                    sources: vec![],
                    stats: serde_json::json!({}),
                    created_unix: crate::now_unix(),
                },
                legs: vec![],
            };
        }
    };
    let mut per_store_stats = Vec::new();
    let mut by_icao: BTreeMap<String, Vec<(usize, FlightLeg)>> = BTreeMap::new();
    for (si, s) in stores.into_iter().enumerate() {
        if si > 0 {
            meta.bbox = meta.bbox.union(&s.meta.bbox);
            meta.t_start = meta.t_start.min(s.meta.t_start);
            meta.t_end = meta.t_end.max(s.meta.t_end);
            for src in &s.meta.sources {
                if !meta.sources.contains(src) {
                    meta.sources.push(src.clone());
                }
            }
        }
        per_store_stats.push(s.meta.stats.clone());
        for l in s.legs {
            by_icao.entry(l.icao.clone()).or_default().push((si, l));
        }
    }
    let mut joins = 0u64;
    let mut out: Vec<FlightLeg> = Vec::new();
    for (icao, mut legs) in by_icao {
        legs.sort_by(|a, b| a.1.t0().total_cmp(&b.1.t0()));
        // groups: (set of store ids, merged leg)
        let mut groups: Vec<(Vec<usize>, FlightLeg)> = Vec::new();
        for (si, l) in legs {
            if let Some((ids, g)) = groups.last_mut() {
                let overlap_or_close = l.t0() <= g.t1() + join_gap_s;
                if overlap_or_close && !ids.contains(&si) {
                    joins += 1;
                    ids.push(si);
                    let mut pts = std::mem::take(&mut g.points);
                    pts.extend(l.points);
                    pts.sort_by(|a, b| {
                        a.t.total_cmp(&b.t)
                            .then((a.flags & pt_flags::MLAT).cmp(&(b.flags & pt_flags::MLAT)))
                    });
                    let mut dd: Vec<TrackPoint> = Vec::with_capacity(pts.len());
                    for p in pts {
                        if let Some(last) = dd.last()
                            && (p.t - last.t).abs() < DEDUPE_S
                        {
                            continue;
                        }
                        dd.push(p);
                    }
                    g.points = dd;
                    if g.typ.is_empty() {
                        g.typ = l.typ.clone();
                        g.engine = l.engine;
                    }
                    if g.reg.is_empty() {
                        g.reg = l.reg.clone();
                    }
                    for part in l.source.split('+') {
                        if !g.source.split('+').any(|x| x == part) {
                            g.source = format!("{}+{part}", g.source);
                        }
                    }
                    continue;
                }
            }
            groups.push((vec![si], l));
        }
        for (n, (_, mut g)) in groups.into_iter().enumerate() {
            let (y, m, d) = utc_date(g.t0());
            g.key = format!("{icao}_{y:04}{m:02}{d:02}_{n}");
            out.push(g);
        }
    }
    out.sort_by(|a, b| a.t0().total_cmp(&b.t0()).then_with(|| a.key.cmp(&b.key)));
    let n_points: usize = out.iter().map(|l| l.points.len()).sum();
    meta.stats = serde_json::json!({
        "legs": out.len(),
        "points": n_points,
        "cross_store_joins": joins,
        "parts": per_store_stats,
    });
    meta.created_unix = crate::now_unix();
    TrackStore { meta, legs: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest_reader;
    use crate::ingest::tests::{T0, gz, pc12_json, spec, tar_of};

    fn mlat_json() -> String {
        // Same aircraft from the MLAT-only feed: one duplicate instant (70030.02) and two
        // extra points between prod rows, baro altitude only.
        let rows = [
            r#"[70030.02, 40.5, -73.9, 5000, 200, 90, 0, 0, null, "mlat", null, null, null, null]"#,
            r#"[70035.0, 40.5, -73.85, 5000, 200, 90, 0, 0, null, "mlat", null, null, null, null]"#,
            r#"[70045.0, 40.5, -73.75, 5000, 200, 90, 2, 0, null, "mlat", null, null, null, null]"#,
        ];
        format!(
            r#"{{"icao":"a1b2c3","t":"PC12","timestamp":{T0},"trace":[{}]}}"#,
            rows.join(",")
        )
    }

    fn other_with_geom() -> String {
        // Another aircraft nearby (>30 min after the PC-12) whose geom-baro offset (+92 m) feeds
        // the regional fallback.
        format!(
            r#"{{"icao":"0000aa","t":"C172","timestamp":{T0},"trace":[[74000.0,40.6,-73.4,3000,100,90,0,0,null,"adsb_icao",3196.85,0,null,null],[74010.0,40.61,-73.4,3000,100,90,0,0,null,"adsb_icao",3196.85,0,null,null]]}}"#
        )
    }

    fn baro_only() -> String {
        format!(
            r#"{{"icao":"0000bb","t":"BE35","timestamp":{T0},"trace":[[74100.0,40.62,-73.41,2000,100,90,0,0,null,"adsb_icao",null,null,null,null],[74110.0,40.63,-73.41,2000,100,90,0,0,null,"adsb_icao",null,null,null,null]]}}"#
        )
    }

    #[test]
    fn merge_split_and_offsets() {
        let sp = spec();
        let prod_tar = tar_of(&[
            ("./traces/c3/trace_full_a1b2c3.json", gz(&pc12_json())),
            ("./traces/aa/trace_full_0000aa.json", gz(&other_with_geom())),
            ("./traces/bb/trace_full_0000bb.json", gz(&baro_only())),
        ]);
        let mlat_tar = tar_of(&[("./traces/c3/trace_full_a1b2c3.json", gz(&mlat_json()))]);
        let p = ingest_reader(std::io::Cursor::new(prod_tar), "prod", &sp, &|_| {}).unwrap();
        let m = ingest_reader(std::io::Cursor::new(mlat_tar), "mlat", &sp, &|_| {}).unwrap();
        let store = build_store(vec![p, m], &sp, vec!["prod".into(), "mlat".into()]);
        let pc: Vec<&FlightLeg> = store.legs.iter().filter(|l| l.icao == "a1b2c3").collect();
        // 9 prod + 3 mlat - 1 duplicate = 11 points, one leg (new-leg flags never cross a >=30 s gap)
        assert_eq!(
            pc.len(),
            1,
            "{:?}",
            pc.iter()
                .map(|l| (&l.key, l.points.len()))
                .collect::<Vec<_>>()
        );
        let leg = pc[0];
        assert_eq!(leg.points.len(), 11);
        assert_eq!(leg.source, "prod+mlat");
        assert_eq!(leg.key, "a1b2c3_20260908_0");
        assert_eq!(store.meta.stats["dedup_dropped"], 1);
        assert_eq!(store.meta.stats["feed_merges"], 1);
        // the duplicate instant kept the prod point (geom present)
        let dup = leg
            .points
            .iter()
            .find(|q| (q.t - (T0 + 70_030.0)).abs() < 0.1)
            .unwrap();
        assert_eq!(dup.alt_src, alt_src::GEOM);
        // leg offset: geom 5100 ft - N(-32) vs baro 5000 ft -> offset = 30.48 + 32 = 62.48 m
        let corr = leg
            .points
            .iter()
            .find(|q| (q.t - (T0 + 70_070.0)).abs() < 0.01)
            .unwrap();
        assert_eq!(corr.alt_src, alt_src::BARO_CORRECTED);
        assert!(
            (corr.alt_m - (5000.0 * 0.3048 + 62.48) as f32).abs() < 0.05,
            "{}",
            corr.alt_m
        );
        // baro-only aircraft gets the regional offset of 0000aa: 3196.85 ft geom - (-32) - 3000 ft
        let bb = store.legs.iter().find(|l| l.icao == "0000bb").unwrap();
        let off = (3196.85 - 3000.0) * 0.3048 + 32.0;
        assert_eq!(bb.points[0].alt_src, alt_src::BARO_CORRECTED);
        assert!((bb.points[0].alt_m - (2000.0 * 0.3048 + off) as f32).abs() < 0.05);
        assert_eq!(store.meta.stats["baro_raw"], 0);
    }

    #[test]
    fn splits_on_gap_and_flag() {
        let sp = spec();
        let rows = [
            r#"[70000.0, 40.5, -73.9, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70010.0, 40.5, -73.8, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70100.0, 40.5, -73.7, 5000, 200, 90, 2, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[70110.0, 40.5, -73.6, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[72000.0, 40.5, -73.5, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
            r#"[72010.0, 40.5, -73.4, 5000, 200, 90, 0, 0, null, "adsb_icao", 5100, 0, null, null]"#,
        ];
        let j = format!(
            r#"{{"icao":"c0ffee","t":"P28A","timestamp":{T0},"trace":[{}]}}"#,
            rows.join(",")
        );
        let tarb = tar_of(&[("./traces/ee/trace_full_c0ffee.json", gz(&j))]);
        let p = ingest_reader(std::io::Cursor::new(tarb), "prod", &sp, &|_| {}).unwrap();
        let store = build_store(vec![p], &sp, vec![]);
        let keys: Vec<(&str, usize)> = store
            .legs
            .iter()
            .map(|l| (l.key.as_str(), l.points.len()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("c0ffee_20260908_0", 2),
                ("c0ffee_20260908_1", 2),
                ("c0ffee_20260908_2", 2)
            ]
        );
        assert_eq!(store.legs[0].source, "prod");
        assert_eq!(store.legs[0].engine, EngineClass::Piston);
    }

    #[test]
    fn merge_across_days() {
        let mk = |t0: f64, n: usize, lbl: &str| {
            let mut st = build_store(vec![], &spec(), vec![lbl.into()]);
            st.legs.push(FlightLeg {
                key: "x".into(),
                icao: "abc123".into(),
                reg: "".into(),
                typ: "PC12".into(),
                source: "prod".into(),
                engine: EngineClass::Turboprop,
                points: (0..n)
                    .map(|i| TrackPoint {
                        t: t0 + i as f64 * 10.0,
                        lat: 40.5,
                        lon: -73.5,
                        alt_m: 1000.0,
                        gs_mps: 60.0,
                        track_deg: 90.0,
                        vrate_mps: 0.0,
                        alt_src: alt_src::GEOM,
                        flags: 0,
                    })
                    .collect(),
            });
            st
        };
        let a = mk(T0 + 86_400.0 - 50.0, 5, "d1"); // ends at 23:59:50
        let b = mk(T0 + 86_400.0 + 5.0, 5, "d2"); // continues after midnight
        let c = mk(T0 + 86_400.0 + 7200.0, 3, "d2b");
        let m = merge_stores(vec![a, b, c], 300.0);
        assert_eq!(m.legs.len(), 2);
        assert_eq!(m.legs[0].points.len(), 10);
        assert_eq!(m.legs[0].key, "abc123_20260908_0");
        assert_eq!(m.legs[1].key, "abc123_20260909_1");
        assert_eq!(m.meta.stats["cross_store_joins"], 1);
        assert_eq!(
            m.meta.sources,
            vec!["d1".to_string(), "d2".to_string(), "d2b".to_string()]
        );
    }
}
