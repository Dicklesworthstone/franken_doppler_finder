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
use crate::ingest::{DayContext, IngestSpec, IngestStats, RawAircraft, RawPoint};
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

/// Union only uninterrupted retained runs. An overlapping run from another feed
/// can supply real reports through a cut; two isolated endpoints cannot do so.
#[derive(Clone, Copy)]
struct RetainedRun {
    start: f64,
    end: f64,
    clipped_before: bool,
    clipped_after: bool,
}

fn retained_coverage(group: &[RawAircraft]) -> Vec<RetainedRun> {
    let mut runs = Vec::new();
    for air in group {
        let Some(first) = air.points.first() else {
            continue;
        };
        let mut run = RetainedRun {
            start: first.t,
            end: first.t,
            clipped_before: first.retained_break,
            clipped_after: first.retained_end,
        };
        for p in &air.points[1..] {
            if p.retained_break || p.t < run.end {
                runs.push(run);
                run = RetainedRun {
                    start: p.t,
                    end: p.t,
                    clipped_before: p.retained_break,
                    clipped_after: p.retained_end,
                };
            } else {
                run.end = p.t;
                run.clipped_after = p.retained_end;
            }
        }
        runs.push(run);
    }
    runs.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut coverage: Vec<RetainedRun> = Vec::new();
    for run in runs {
        if let Some(last) = coverage.last_mut()
            && (run.start <= last.end || (!last.clipped_after && !run.clipped_before))
        {
            if run.start == last.start {
                last.clipped_before &= run.clipped_before;
            }
            if run.end > last.end {
                last.end = run.end;
                last.clipped_after = run.clipped_after;
            } else if run.end == last.end {
                last.clipped_after &= run.clipped_after;
            }
        } else {
            coverage.push(run);
        }
    }
    coverage
}

/// A gap is checked only when it is at least NEW_LEG_MIN_GAP_S long. Ground
/// observations no farther apart than that can therefore be represented by
/// their first/last event without changing whether any checked gap contains one.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BoundaryEvents {
    ground: Vec<(f64, f64)>,
    new_leg: Vec<f64>,
}

impl BoundaryEvents {
    pub(crate) fn push(&mut self, t: f64, ground: bool) {
        if ground {
            if let Some(last) = self.ground.last_mut()
                && t >= last.1
                && t - last.1 <= NEW_LEG_MIN_GAP_S
            {
                last.1 = t;
            } else {
                self.ground.push((t, t));
            }
        } else {
            self.new_leg.push(t);
        }
    }

    pub(crate) fn normalize(&mut self) {
        self.new_leg.retain(|t| t.is_finite());
        self.new_leg.sort_by(f64::total_cmp);
        self.new_leg.dedup();
        self.ground
            .retain(|(a, b)| a.is_finite() && b.is_finite() && a <= b);
        self.ground.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut compact: Vec<(f64, f64)> = Vec::new();
        for &(start, end) in &self.ground {
            if let Some(last) = compact.last_mut()
                && start - last.1 <= NEW_LEG_MIN_GAP_S
            {
                last.1 = last.1.max(end);
            } else {
                compact.push((start, end));
            }
        }
        self.ground = compact;
    }

    fn between(&self, a: f64, b: f64) -> bool {
        debug_assert!(b - a >= NEW_LEG_MIN_GAP_S);
        let ground = self.ground.partition_point(|x| x.1 < a);
        let airborne = self.new_leg.partition_point(|t| *t <= a);
        self.ground.get(ground).is_some_and(|x| x.0 <= b)
            || self.new_leg.get(airborne).is_some_and(|t| *t <= b)
    }

    fn remap_duplicates(&mut self, duplicate_times: &HashMap<u64, f64>) {
        for t in &mut self.new_leg {
            while let Some(&retained_t) = duplicate_times.get(&t.to_bits()) {
                *t = retained_t;
            }
        }
        self.normalize();
    }
}

type LegBoundaries = BTreeMap<String, BoundaryEvents>;

/// Keep the compact point hints for consumers, but retain exact event timestamps
/// in metadata: a different feed can insert reports on either side of an event.
fn mark_boundaries(points: &mut [TrackPoint], boundaries: &BoundaryEvents) {
    for p in points.iter_mut() {
        p.flags &= !(pt_flags::NEW_LEG | pt_flags::NEW_LEG_AFTER);
    }
    for i in 1..points.len() {
        if points[i].t - points[i - 1].t >= NEW_LEG_MIN_GAP_S
            && boundaries.between(points[i - 1].t, points[i].t)
        {
            points[i - 1].flags |= pt_flags::NEW_LEG_AFTER;
            points[i].flags |= pt_flags::NEW_LEG;
        }
    }
    if let Some(first) = points.first_mut()
        && (boundaries.ground.first().is_some_and(|b| b.0 <= first.t)
            || boundaries.new_leg.first().is_some_and(|t| *t <= first.t))
    {
        first.flags |= pt_flags::NEW_LEG;
    }
    if let Some(last) = points.last_mut()
        && (boundaries.ground.last().is_some_and(|b| b.1 >= last.t)
            || boundaries.new_leg.last().is_some_and(|t| *t > last.t))
    {
        last.flags |= pt_flags::NEW_LEG_AFTER;
    }
}

/// Merge raw aircraft from any number of ingests (feeds, days) into a store.
pub fn build_store(
    parts: Vec<(Vec<RawAircraft>, IngestStats)>,
    spec: &IngestSpec,
    sources: Vec<String>,
) -> TrackStore {
    build_store_inner(parts, spec, sources, None)
}

pub(crate) fn build_day_store(
    parts: Vec<(Vec<RawAircraft>, IngestStats)>,
    spec: &IngestSpec,
    sources: Vec<String>,
    context: DayContext,
) -> TrackStore {
    build_store_inner(parts, spec, sources, Some(context))
}

fn build_store_inner(
    parts: Vec<(Vec<RawAircraft>, IngestStats)>,
    spec: &IngestSpec,
    sources: Vec<String>,
    context: Option<DayContext>,
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
    let mut leg_boundaries = LegBoundaries::new();
    for (icao, mut group) in by_icao {
        if group.len() > 1 {
            merges += 1;
        }
        group.sort_by_key(|a| feed_of(&a.feed).rank());
        let typ = group
            .iter()
            .filter(|a| !a.points.is_empty())
            .map(|a| a.typ.as_str())
            .find(|t| !t.is_empty())
            .unwrap_or("")
            .to_string();
        let reg = group
            .iter()
            .filter(|a| !a.points.is_empty())
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
        let coverage = retained_coverage(&group);
        let mut boundaries = BoundaryEvents::default();
        for a in &group {
            boundaries.ground.extend(&a.leg_boundaries.ground);
            boundaries.new_leg.extend(&a.leg_boundaries.new_leg);
        }
        boundaries.normalize();
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
        let mut duplicate_times = HashMap::new();
        for q in pts {
            if let Some(last) = dd.last_mut()
                && (q.p.t - last.p.t).abs() < DEDUPE_S
            {
                dedup_dropped += 1;
                if q.rank < last.rank {
                    if last.p.t != q.p.t {
                        duplicate_times.insert(last.p.t.to_bits(), q.p.t);
                    }
                    let keep_leg = last.p.new_leg;
                    *last = q;
                    last.p.new_leg |= keep_leg;
                } else {
                    if q.p.t != last.p.t {
                        duplicate_times.insert(q.p.t.to_bits(), last.p.t);
                    }
                    last.p.new_leg |= q.p.new_leg;
                }
                continue;
            }
            dd.push(q);
        }
        boundaries.remap_duplicates(&duplicate_times);
        // split into legs
        let mut cur: Vec<MPoint> = Vec::new();
        let flush = |cur: &mut Vec<MPoint>, drafts: &mut Vec<LegDraft>| {
            // A lone midnight report can become a usable segment with the next
            // archive's report. Final filtering still requires at least two.
            if cur.len() >= 2 || (context.is_some() && !cur.is_empty()) {
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
        let mut component = 0;
        let components: Vec<_> = dd
            .iter()
            .map(|q| {
                while component + 1 < coverage.len() && q.p.t > coverage[component].end {
                    component += 1;
                }
                component
            })
            .collect();
        for i in 0..dd.len() {
            let run = coverage[components[i]];
            dd[i].p.retained_break =
                run.clipped_before && (i == 0 || components[i - 1] != components[i]);
            dd[i].p.retained_end =
                run.clipped_after && (i + 1 == dd.len() || components[i + 1] != components[i]);
        }
        component = 0;
        for (q, next_component) in dd.into_iter().zip(components) {
            let previous_component = component;
            component = next_component;
            if let Some(last) = cur.last() {
                let gap = q.p.t - last.p.t;
                if component != previous_component
                    || gap > LEG_GAP_S
                    || (gap >= NEW_LEG_MIN_GAP_S && boundaries.between(last.p.t, q.p.t))
                {
                    flush(&mut cur, &mut drafts);
                }
            }
            cur.push(q);
        }
        flush(&mut cur, &mut drafts);
        leg_boundaries.insert(icao, boundaries);
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
                flags: (p.flags & (pt_flags::MLAT | pt_flags::TISB | pt_flags::STALE))
                    | if p.retained_break {
                        pt_flags::CLIPPED_BEFORE
                    } else {
                        0
                    }
                    | if p.retained_end {
                        pt_flags::CLIPPED_AFTER
                    } else {
                        0
                    },
            });
        }
        mark_boundaries(&mut points, &leg_boundaries[&d.icao]);
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
    let mut meta = StoreMeta {
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
            "leg_boundaries": leg_boundaries,
        }),
        created_unix: crate::now_unix(),
    };
    if let Some(context) = context {
        meta.stats["midnight_context"] = serde_json::json!(context);
    }
    TrackStore { meta, legs }
}

/// Prefer non-MLAT reports inside the surviving original duplicate intervals. Keep the exact
/// legacy representative when quality ties: a report discarded during an earlier merge must
/// not reappear merely because a later store shifts the interval anchors. The raw point array
/// is append-only until these representative indices have been materialized.
fn dedupe_store_points(
    mut points: Vec<TrackPoint>,
    anchors: Vec<usize>,
    duplicate_times: &mut HashMap<u64, f64>,
) -> Vec<TrackPoint> {
    let mut kept: Vec<TrackPoint> = anchors.into_iter().map(|i| points[i]).collect();
    points.sort_by(|a, b| {
        a.t.total_cmp(&b.t)
            .then((a.flags & pt_flags::MLAT).cmp(&(b.flags & pt_flags::MLAT)))
    });
    let mut reports = points.into_iter().peekable();
    for preferred in &mut kept {
        let group_start = preferred.t;
        // Some reports were already discarded before a later store changed the anchors.
        // They remain discarded unless they belong to a surviving original interval.
        while reports
            .peek()
            .is_some_and(|p| p.t.total_cmp(&group_start).is_lt())
        {
            let _ = reports.next();
        }
        while let Some(p) = reports.next_if(|p| (p.t - group_start).abs() < DEDUPE_S) {
            let boundary =
                (preferred.flags | p.flags) & (pt_flags::NEW_LEG | pt_flags::NEW_LEG_AFTER);
            let clipped =
                (preferred.flags & p.flags) & (pt_flags::CLIPPED_BEFORE | pt_flags::CLIPPED_AFTER);
            if (p.flags & pt_flags::MLAT) < (preferred.flags & pt_flags::MLAT) {
                if preferred.t != p.t {
                    duplicate_times.insert(preferred.t.to_bits(), p.t);
                }
                *preferred = p;
            } else if p.t != preferred.t {
                duplicate_times.insert(p.t.to_bits(), preferred.t);
            }
            preferred.flags = (preferred.flags
                & !(pt_flags::CLIPPED_BEFORE | pt_flags::CLIPPED_AFTER))
                | boundary
                | clipped;
        }
    }
    kept
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
    let mut contexts = Vec::new();
    let mut leg_boundaries = LegBoundaries::new();
    let mut by_icao: BTreeMap<String, Vec<(usize, FlightLeg)>> = BTreeMap::new();
    for (si, s) in stores.into_iter().enumerate() {
        contexts.push(DayContext::from_stats(&s.meta.stats));
        let stored_boundaries: LegBoundaries = s
            .meta
            .stats
            .get("leg_boundaries")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        for (icao, boundaries) in &stored_boundaries {
            let all = leg_boundaries.entry(icao.clone()).or_default();
            all.ground.extend(&boundaries.ground);
            all.new_leg.extend(&boundaries.new_leg);
        }
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
            if !stored_boundaries.contains_key(&l.icao) {
                let boundaries = leg_boundaries.entry(l.icao.clone()).or_default();
                for p in &l.points {
                    if p.flags & pt_flags::NEW_LEG != 0 {
                        boundaries.push(p.t, false);
                    }
                    if p.flags & pt_flags::NEW_LEG_AFTER != 0 {
                        boundaries.push(p.t, true);
                    }
                }
            }
            by_icao.entry(l.icao.clone()).or_default().push((si, l));
        }
    }
    for boundaries in leg_boundaries.values_mut() {
        boundaries.normalize();
    }
    let mut joins = 0u64;
    let mut out: Vec<FlightLeg> = Vec::new();
    for (icao, mut legs) in by_icao {
        let boundaries = leg_boundaries.get_mut(&icao).unwrap();
        legs.sort_by(|a, b| a.1.t0().total_cmp(&b.1.t0()));
        // groups: (set of store ids, leg, original representative indices). Allocate indices
        // only for an actual join. They preserve the old join boundaries, duplicate groups
        // and exact-timestamp payloads while raw points stay available for quality selection.
        // An unjoined leg keeps its original allocation and never runs the duplicate pass.
        let mut groups: Vec<(Vec<usize>, FlightLeg, Option<Vec<usize>>)> = Vec::new();
        for (si, l) in legs {
            if let Some((ids, g, anchors)) = groups.last_mut() {
                let last_t = anchors.as_ref().map_or_else(
                    || g.t1(),
                    |indices| indices.last().map_or(f64::NAN, |&i| g.points[i].t),
                );
                let overlap_or_close = l.t0() <= last_t + join_gap_s;
                let gap = l.t0() - last_t;
                let new_leg = gap >= NEW_LEG_MIN_GAP_S && boundaries.between(last_t, l.t0());
                let clipped_end = g
                    .points
                    .iter()
                    .filter(|p| p.t >= last_t && p.t - last_t < DEDUPE_S)
                    .all(|p| p.flags & pt_flags::CLIPPED_AFTER != 0);
                let clipped_start = l
                    .points
                    .first()
                    .is_some_and(|p| p.flags & pt_flags::CLIPPED_BEFORE != 0);
                // Natural source endpoints retain the established gap policy.
                // A gap created by pruning needs proof that both archives kept
                // the closest reports at the UTC seam; absent proof never
                // overrides an explicit cut (including re-filtered stores).
                let covered_gap = gap <= 0.0
                    || !(clipped_end || clipped_start)
                    || match contexts[si] {
                        Some(context) if ids.iter().all(|&id| contexts[id].is_some()) => {
                            context.covers(last_t, l.t0())
                                && ids.iter().any(|&id| {
                                    contexts[id].is_some_and(|c| c.covers(last_t, l.t0()))
                                })
                        }
                        _ => false,
                    };
                if overlap_or_close && !new_leg && covered_gap && !ids.contains(&si) {
                    joins += 1;
                    ids.push(si);
                    let previous_len = g.points.len();
                    let anchors = anchors.get_or_insert_with(|| (0..previous_len).collect());
                    g.points.extend(l.points);
                    anchors.extend(previous_len..g.points.len());
                    anchors.sort_by(|&i, &j| {
                        let (a, b) = (&g.points[i], &g.points[j]);
                        a.t.total_cmp(&b.t)
                            .then((a.flags & pt_flags::MLAT).cmp(&(b.flags & pt_flags::MLAT)))
                    });
                    anchors.dedup_by(|b, a| (g.points[*b].t - g.points[*a].t).abs() < DEDUPE_S);
                    // An unrecognized designator is no stronger than a missing one.
                    // Prefer a recognized adjoining-day type without changing the
                    // first recognized type's precedence over another known type.
                    if g.typ.is_empty()
                        || (g.engine == EngineClass::Unknown && l.engine != EngineClass::Unknown)
                    {
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
            groups.push((vec![si], l, None));
        }
        let mut n = 0;
        for (_, mut g, anchors) in groups {
            if let Some(anchors) = anchors {
                let mut duplicate_times = HashMap::new();
                g.points = dedupe_store_points(
                    std::mem::take(&mut g.points),
                    anchors,
                    &mut duplicate_times,
                );
                boundaries.remap_duplicates(&duplicate_times);
            }
            mark_boundaries(&mut g.points, boundaries);
            // An overlapping archive may reveal a real break inside the merged
            // span. Preserve it even when its duplicate position lost on quality.
            let mut pending = Some(g);
            while let Some(mut part) = pending.take() {
                if let Some(cut) = part.points.windows(2).position(|p| {
                    p[1].t - p[0].t >= NEW_LEG_MIN_GAP_S && boundaries.between(p[0].t, p[1].t)
                }) {
                    let tail = part.points.split_off(cut + 1);
                    pending = Some(FlightLeg {
                        key: String::new(),
                        icao: part.icao.clone(),
                        reg: part.reg.clone(),
                        typ: part.typ.clone(),
                        source: part.source.clone(),
                        engine: part.engine,
                        points: tail,
                    });
                }
                let (y, m, d) = utc_date(part.t0());
                part.key = format!("{icao}_{y:04}{m:02}{d:02}_{n}");
                n += 1;
                out.push(part);
            }
        }
    }
    out.sort_by(|a, b| a.t0().total_cmp(&b.t0()).then_with(|| a.key.cmp(&b.key)));
    let n_points: usize = out.iter().map(|l| l.points.len()).sum();
    meta.stats = serde_json::json!({
        "legs": out.len(),
        "points": n_points,
        "cross_store_joins": joins,
        "parts": per_store_stats,
        "leg_boundaries": leg_boundaries,
    });
    meta.created_unix = crate::now_unix();
    TrackStore { meta, legs: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest_reader;
    use crate::ingest::tests::{T0, gz, pc12_json, spec, tar_of};

    fn seam_spec(day: f64) -> IngestSpec {
        IngestSpec {
            bbox: fdop_types::BBox::new(0.0, 0.0, 0.1, 0.1),
            t_start: day - 3600.0,
            t_end: day + 86_400.0 + 3600.0,
            geoid_n_m: 0.0,
            keep_unknown: true,
        }
    }

    fn seam_row(t: f64, lon: f64) -> serde_json::Value {
        serde_json::json!([
            t,
            0.05,
            lon,
            4000,
            200,
            90,
            0,
            0,
            null,
            "adsb_icao",
            4100,
            0
        ])
    }

    fn seam_tar(base: f64, rows: &[serde_json::Value]) -> Vec<u8> {
        let json = serde_json::json!({"icao":"000001", "t":"PC12", "timestamp":base, "trace":rows});
        tar_of(&[("trace_full_000001.json", gz(&json.to_string()))])
    }

    fn seam_part(
        day: f64,
        base: f64,
        feed: &str,
        rows: &[serde_json::Value],
    ) -> (Vec<RawAircraft>, IngestStats) {
        crate::ingest::ingest_day_reader(
            std::io::Cursor::new(seam_tar(base, rows)),
            feed,
            &seam_spec(day),
            &|_| {},
            DayContext::new(day, 300.0, 3600.0),
        )
        .unwrap()
    }

    fn seam_day(day: f64, parts: Vec<(Vec<RawAircraft>, IngestStats)>) -> TrackStore {
        build_day_store(
            parts,
            &seam_spec(day),
            vec!["synthetic".into()],
            DayContext::new(day, 300.0, 3600.0),
        )
    }

    fn seam_roundtrip(store: &TrackStore, label: &str) -> TrackStore {
        let path = std::env::temp_dir().join(format!(
            "fdop-seam-{label}-{}-{}.fdtrk",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        store.save(&path).unwrap();
        TrackStore::load(&path).unwrap()
    }

    #[test]
    fn compact_ground_events_preserve_every_split_eligible_gap() {
        let ground = [0.0, 15.0, 30.0, 60.0, 91.0, 121.0, 151.0, 300.0];
        let airborne = [8.0, 98.0, 200.0];
        let mut events = BoundaryEvents::default();
        for t in ground.into_iter().rev() {
            events.push(t, true);
        }
        for t in airborne {
            events.push(t, false);
        }
        events.normalize();
        assert_eq!(events.ground, [(0.0, 60.0), (91.0, 151.0), (300.0, 300.0)]);
        for i in -80..=640 {
            let a = i as f64 * 0.5;
            for gap in [30.0, 30.0001, 45.0, 60.0, 300.0] {
                let b = a + gap;
                let expected = ground.into_iter().any(|t| a <= t && t <= b)
                    || airborne.into_iter().any(|t| a < t && t <= b);
                assert_eq!(events.between(a, b), expected, "[{a}, {b}]");
            }
        }
        let mut dense = BoundaryEvents::default();
        for i in 0..10_000 {
            dense.push(i as f64 * 0.1, true);
        }
        assert_eq!(
            dense.ground.len(),
            1,
            "dense reports must compact during ingest, before day build"
        );
    }

    #[test]
    fn raw_midnight_crossing_survives_day_cache_roundtrip() {
        for midnight in [T0, T0 + 86_400.0] {
            for single in [false, true] {
                let mut left = vec![seam_row(-70.0, -0.02), seam_row(-60.0, -0.01)];
                let mut right = vec![seam_row(60.0, 0.11), seam_row(70.0, 0.12)];
                if single {
                    left.remove(0);
                    right.pop();
                }
                let mut cached = Vec::new();
                for (day, rows) in [(midnight - 86_400.0, left), (midnight, right)] {
                    let sp = seam_spec(day);
                    let legacy = ingest_reader(
                        std::io::Cursor::new(seam_tar(midnight, &rows)),
                        "prod",
                        &sp,
                        &|_| {},
                    )
                    .unwrap();
                    assert!(legacy.0.is_empty(), "neither archive alone reaches the box");
                    let fresh = seam_day(day, vec![seam_part(day, midnight, "prod", &rows)]);
                    assert_eq!(fresh.n_points(), rows.len());
                    let loaded = seam_roundtrip(&fresh, "crossing");
                    assert!(
                        DayContext::from_stats(&loaded.meta.stats)
                            .unwrap()
                            .covers(midnight - 60.0, midnight + 60.0)
                    );
                    cached.push(loaded);
                }
                let merged = merge_stores(cached, 300.0);
                let cropped =
                    merged.filtered(&seam_spec(midnight).bbox, midnight - 1.0, midnight + 1.0);
                assert_eq!(
                    cropped.legs.len(),
                    1,
                    "single={single}, midnight={midnight}"
                );
                assert_eq!(cropped.n_points(), if single { 2 } else { 4 });
                assert_eq!(
                    cropped.legs[0].points[usize::from(!single)].t,
                    midnight - 60.0
                );
                assert_eq!(cropped.legs[0].typ, "PC12");
                assert!(cropped.meta.stats.get("midnight_context").is_none());
            }
        }
    }

    #[test]
    fn midnight_boundaries_survive_ground_only_feeds_and_interleaving() {
        let midnight = T0 + 86_400.0;
        for marker in [
            serde_json::json!([-10, null, null, "ground"]),
            serde_json::json!([-10, null, null, null, null, null, 2]),
        ] {
            for marker_only_feed in [false, true] {
                let mut prod = vec![seam_row(-100.0, -0.03), seam_row(-90.0, -0.02)];
                let mut mlat = vec![seam_row(-85.0, -0.01)];
                if marker_only_feed {
                    mlat = vec![marker.clone()];
                } else {
                    prod.push(marker.clone());
                }
                let m = if marker_only_feed {
                    // A higher-priority ground-only source must not classify
                    // the actual airborne source as unknown or supply its reg.
                    let json = serde_json::json!({"icao":"000001", "t":"ZZZZ9", "r":"BOUNDARY_ONLY",
                        "timestamp":midnight, "trace":mlat});
                    crate::ingest::ingest_day_reader(
                        std::io::Cursor::new(tar_of(&[(
                            "trace_full_000001.json",
                            gz(&json.to_string()),
                        )])),
                        "prod",
                        &seam_spec(midnight - 86_400.0),
                        &|_| {},
                        DayContext::new(midnight - 86_400.0, 300.0, 3600.0),
                    )
                    .unwrap()
                } else {
                    seam_part(midnight - 86_400.0, midnight, "mlat", &mlat)
                };
                assert_eq!(
                    m.0.len(),
                    1,
                    "ground-only companion evidence must survive ingest"
                );
                let left = seam_day(
                    midnight - 86_400.0,
                    vec![
                        seam_part(
                            midnight - 86_400.0,
                            midnight,
                            if marker_only_feed { "mlat" } else { "prod" },
                            &prod,
                        ),
                        m,
                    ],
                );
                assert!(
                    left.legs
                        .iter()
                        .all(|leg| leg.typ == "PC12" && leg.reg.is_empty())
                );
                let right = seam_day(
                    midnight,
                    vec![seam_part(
                        midnight,
                        midnight,
                        "prod",
                        &[seam_row(20.0, 0.11), seam_row(30.0, 0.12)],
                    )],
                );
                let merged = merge_stores(
                    vec![
                        seam_roundtrip(&left, "ground-left"),
                        seam_roundtrip(&right, "ground-right"),
                    ],
                    300.0,
                );
                assert_eq!(merged.legs.len(), 2);
                assert!(
                    merged
                        .filtered(
                            &seam_spec(midnight).bbox,
                            midnight - 200.0,
                            midnight + 200.0
                        )
                        .legs
                        .is_empty()
                );
            }
        }
        // The second day's archive itself supplies the later pre-ground report.
        // A flag left on the first archive's terminal -90s point is insufficient.
        let left = seam_day(
            midnight - 86_400.0,
            vec![seam_part(
                midnight - 86_400.0,
                midnight,
                "prod",
                &[
                    seam_row(-100.0, -0.03),
                    seam_row(-90.0, -0.02),
                    serde_json::json!([-10, null, null, "ground"]),
                ],
            )],
        );
        let right = seam_day(
            midnight,
            vec![seam_part(
                midnight,
                midnight,
                "mlat",
                &[
                    seam_row(-85.0, -0.01),
                    seam_row(20.0, 0.11),
                    seam_row(30.0, 0.12),
                ],
            )],
        );
        let merged = merge_stores(
            vec![
                seam_roundtrip(&left, "overlap-left"),
                seam_roundtrip(&right, "overlap-right"),
            ],
            300.0,
        );
        assert_eq!(merged.legs.len(), 2);
        assert_eq!(merged.legs[0].t1(), midnight - 85.0);
        assert_eq!(merged.legs[1].t0(), midnight + 20.0);
    }

    #[test]
    fn feed_handoffs_stay_continuous_but_only_real_coverage_bridges_pruning() {
        let day = T0;
        let read = |rows: Vec<serde_json::Value>, feed| seam_part(day, day, feed, &rows);
        let natural = seam_day(
            day,
            vec![
                read(vec![seam_row(10.0, 0.05), seam_row(20.0, 0.05)], "prod"),
                read(vec![seam_row(25.0, 0.05), seam_row(30.0, 0.05)], "mlat"),
            ],
        );
        assert_eq!(natural.legs.len(), 1);
        assert_eq!(natural.n_points(), 4);

        // A long northward detour is discarded between two visits. Its artificial
        // retained endpoints must not become an airborne segment across the box.
        let rows: Vec<_> = (0..20)
            .map(|i| {
                let lat = if i <= 4 || i >= 15 { 0.05 } else { 1.0 };
                let lon = match i {
                    3 => -0.04,
                    4 => -0.05,
                    15 => 0.15,
                    16 => 0.14,
                    _ if i < 10 => -0.02,
                    _ => 0.12,
                };
                serde_json::json!([1000 + i * 10, lat, lon, 4000, 200, 90, 0, 0])
            })
            .collect();
        let wide = IngestSpec {
            bbox: fdop_types::BBox::new(0.0, -0.03, 0.1, 0.13),
            ..seam_spec(day)
        };
        let raw = ingest_reader(
            std::io::Cursor::new(seam_tar(day, &rows)),
            "prod",
            &wide,
            &|_| {},
        )
        .unwrap();
        assert!(raw.0[0].points.iter().any(|p| p.retained_break));
        let disconnected = build_store(vec![raw.clone()], &wide, vec![]);
        assert_eq!(disconnected.legs.len(), 2);
        assert!(
            disconnected
                .filtered(&seam_spec(day).bbox, day + 1080.0, day + 1120.0)
                .legs
                .is_empty()
        );
        let bridge_rows: Vec<_> = (3..=16)
            .map(|i| seam_row(1000.0 + i as f64 * 10.0, 0.05))
            .collect();
        let bridge = ingest_reader(
            std::io::Cursor::new(seam_tar(day, &bridge_rows)),
            "mlat",
            &wide,
            &|_| {},
        )
        .unwrap();
        let connected = build_store(vec![raw, bridge], &wide, vec![]);
        assert_eq!(
            connected.legs.len(),
            1,
            "an uninterrupted overlapping source proves real coverage"
        );
        assert!(
            !connected
                .filtered(&seam_spec(day).bbox, day + 1080.0, day + 1120.0)
                .legs
                .is_empty()
        );
    }

    #[test]
    fn explicit_clipping_without_day_proof_blocks_cross_store_chords() {
        let mut left = merge_test_store(
            vec![
                merge_test_point(0.0, 0),
                merge_test_point(10.0, pt_flags::CLIPPED_AFTER),
            ],
            "a",
        );
        let mut right = merge_test_store(
            vec![
                merge_test_point(20.0, pt_flags::CLIPPED_BEFORE),
                merge_test_point(30.0, 0),
            ],
            "b",
        );
        assert_eq!(
            merge_stores(vec![left.clone(), right.clone()], 300.0)
                .legs
                .len(),
            2
        );
        // Even a valid marker cannot certify an arbitrary same-side pruning gap.
        left.meta.stats["midnight_context"] =
            serde_json::json!(DayContext::new(0.0, 300.0, 3600.0));
        right.meta.stats["midnight_context"] = left.meta.stats["midnight_context"].clone();
        assert_eq!(merge_stores(vec![left, right], 300.0).legs.len(), 2);
    }

    #[test]
    fn duplicate_new_leg_evidence_keeps_preferred_geometry_and_original_gap_rule() {
        let day = T0;
        let prod = [
            seam_row(0.0, 0.05),
            seam_row(10.0, 0.05),
            seam_row(100.0, 0.05),
            seam_row(110.0, 0.05),
        ];
        let mut marker = seam_row(100.02, 0.06);
        marker[6] = serde_json::json!(2);
        marker[9] = serde_json::json!("mlat");
        let store = seam_day(
            day,
            vec![
                seam_part(day, day, "prod", &prod),
                seam_part(day, day, "mlat", &[marker]),
            ],
        );
        assert_eq!(store.legs.len(), 2);
        assert_eq!(store.legs[1].t0(), day + 100.0);
        assert_eq!(store.legs[1].points[0].lon, 0.05);
        assert_eq!(store.legs[1].points[0].flags & pt_flags::MLAT, 0);
        let short = seam_day(
            day,
            vec![seam_part(
                day,
                day,
                "prod",
                &[
                    seam_row(0.0, 0.05),
                    serde_json::json!([10, null, null, "ground"]),
                    seam_row(20.0, 0.05),
                ],
            )],
        );
        assert_eq!(
            short.legs.len(),
            1,
            "the established 30-second boundary rule is unchanged"
        );
    }

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

    fn merge_test_point(t: f64, flags: u8) -> TrackPoint {
        TrackPoint {
            t,
            lat: 40.876466,
            lon: -73.524951,
            alt_m: 1000.0,
            gs_mps: 90.0,
            track_deg: 90.0,
            vrate_mps: 0.0,
            alt_src: alt_src::GEOM,
            flags,
        }
    }

    fn merge_test_store(points: Vec<TrackPoint>, label: &str) -> TrackStore {
        TrackStore {
            meta: StoreMeta {
                version: STORE_VERSION,
                bbox: fdop_types::BBox::new(40.0, -74.0, 41.0, -73.0),
                t_start: points.first().unwrap().t,
                t_end: points.last().unwrap().t,
                geoid_n_m: -32.0,
                sources: vec![label.into()],
                stats: serde_json::json!({}),
                created_unix: T0,
            },
            legs: vec![FlightLeg {
                key: label.into(),
                icao: "abc123".into(),
                reg: "TEST".into(),
                typ: "PC12".into(),
                source: "prod+mlat".into(),
                engine: EngineClass::Turboprop,
                points,
            }],
        }
    }

    #[test]
    fn cross_day_type_recovery_keeps_first_recognized_type() {
        for (first_known, later_known) in [("PC12", "C172"), ("C172", "PC12")] {
            let stores = ["ZZZZ9", first_known, later_known]
                .into_iter()
                .enumerate()
                .map(|(i, typ)| {
                    let start = T0 + i as f64 * 20.0;
                    let mut store = merge_test_store(
                        vec![
                            merge_test_point(start, 0),
                            merge_test_point(start + 10.0, 0),
                        ],
                        typ,
                    );
                    store.legs[0].typ = typ.into();
                    store.legs[0].engine = engine_class(typ);
                    store
                })
                .collect();
            let merged = merge_stores(stores, 300.0);
            assert_eq!(merged.legs.len(), 1);
            assert_eq!(merged.legs[0].points.len(), 6);
            assert_eq!(merged.legs[0].typ, first_known);
            assert_eq!(merged.legs[0].engine, engine_class(first_known));
        }
    }

    #[test]
    fn cross_day_merge_keeps_non_mlat_geometry() {
        let midnight = T0 + 86_400.0;
        let point = |offset: f64, mlat: bool| {
            let mut p = merge_test_point(midnight + offset, 0);
            // A 90 m/s eastbound flight. The duplicate MLAT reports have an independent
            // 1.1 km northward error and a 200 m height error; they must not bend its seam.
            p.lon +=
                (90.0 * offset / fdop_types::geo::EARTH_R).to_degrees() / p.lat.to_radians().cos();
            if mlat {
                p.lat += 0.01;
                p.alt_m -= 200.0;
                p.alt_src = alt_src::BARO_RAW;
                p.flags = pt_flags::MLAT;
            }
            p
        };
        for offset in [-0.03125, 0.0, 0.03125] {
            for reverse_stores in [false, true] {
                let a = merge_test_store(
                    vec![
                        point(-10.0, false),
                        point(offset, true),
                        point(10.0 + offset, true),
                    ],
                    "day1",
                );
                let b = merge_test_store(
                    vec![point(0.0, false), point(10.0, false), point(20.0, false)],
                    "day2",
                );
                let stores = if reverse_stores {
                    vec![b, a]
                } else {
                    vec![a, b]
                };
                let merged = merge_stores(stores, 300.0);
                assert_eq!(merged.legs.len(), 1);
                let leg = &merged.legs[0];
                let expected: Vec<_> = [-10.0, 0.0, 10.0, 20.0]
                    .into_iter()
                    .map(|t| point(t, false))
                    .collect();
                assert_eq!(
                    leg.points, expected,
                    "MLAT offset={offset}, reverse_stores={reverse_stores}"
                );
                assert_eq!(leg.key, "abc123_20260908_0");
                assert_eq!(leg.icao, "abc123");
                assert_eq!(leg.reg, "TEST");
                assert_eq!(leg.typ, "PC12");
                assert_eq!(merged.meta.stats["cross_store_joins"], 1);
            }
        }
    }

    #[test]
    fn cross_store_duplicate_priority_keeps_cluster_boundaries() {
        let p = merge_test_point;
        let merged = merge_stores(
            vec![
                merge_test_store(vec![p(0.0, pt_flags::MLAT), p(0.0625, 0)], "a"),
                merge_test_store(vec![p(0.03125, 0), p(10.0, 0)], "b"),
                merge_test_store(vec![p(5.0, 0), p(20.0, 0)], "c"),
            ],
            300.0,
        );
        // A later, preferred representative does not extend the original 0..0.05 group,
        // either immediately or when the already merged leg is joined to a third store.
        assert_eq!(
            merged.legs[0].points,
            [
                p(0.03125, 0),
                p(0.0625, 0),
                p(5.0, 0),
                p(10.0, 0),
                p(20.0, 0)
            ]
        );
        let at_boundary = merge_stores(
            vec![
                merge_test_store(vec![p(0.0, pt_flags::MLAT), p(10.0, 0)], "a"),
                merge_test_store(vec![p(DEDUPE_S, 0), p(20.0, 0)], "b"),
            ],
            300.0,
        );
        assert_eq!(
            at_boundary.legs[0].points,
            [
                p(0.0, pt_flags::MLAT),
                p(DEDUPE_S, 0),
                p(10.0, 0),
                p(20.0, 0)
            ]
        );
    }

    #[test]
    fn cross_store_priority_preserves_join_boundary() {
        let p = merge_test_point;
        let merged = merge_stores(
            vec![
                merge_test_store(vec![p(-10.0, 0), p(0.0, pt_flags::MLAT)], "a"),
                merge_test_store(vec![p(-5.0, 0), p(0.03125, 0)], "b"),
                merge_test_store(vec![p(300.015625, 0), p(320.0, 0)], "c"),
            ],
            300.0,
        );
        // The preferred point is later, but choosing it does not close a gap that the
        // original duplicate group left beyond the join limit.
        assert_eq!(merged.legs.len(), 2);
        assert_eq!(
            merged.legs[0].points,
            [p(-10.0, 0), p(-5.0, 0), p(0.03125, 0)]
        );
        assert_eq!(merged.legs[1].points, [p(300.015625, 0), p(320.0, 0)]);
        assert_eq!(merged.meta.stats["cross_store_joins"], 1);
    }

    #[test]
    fn cross_store_priority_handles_interleaved_reports() {
        let p = merge_test_point;
        let merged = merge_stores(
            vec![
                merge_test_store(
                    vec![
                        p(-10.0, 0),
                        p(0.0, pt_flags::MLAT),
                        p(0.0625, 0),
                        p(10.0, 0),
                    ],
                    "a",
                ),
                merge_test_store(vec![p(-5.0, 0), p(0.046875, 0), p(10.0, 0)], "b"),
                merge_test_store(
                    vec![
                        p(-1.0, 0),
                        p(-0.03125, pt_flags::MLAT),
                        p(0.03125, 0),
                        p(20.0, 0),
                    ],
                    "c",
                ),
            ],
            300.0,
        );
        // The third store introduces a point before an earlier duplicate group. Preserve
        // the surviving original intervals, rather than moving a preferred report into an
        // older group and emitting it out of order or outside its 0.05 s interval.
        assert_eq!(
            merged.legs[0].points,
            [
                p(-10.0, 0),
                p(-5.0, 0),
                p(-1.0, 0),
                p(-0.03125, pt_flags::MLAT),
                p(0.03125, 0),
                p(10.0, 0),
                p(20.0, 0),
            ]
        );
        assert!(merged.legs[0].points.windows(2).all(|p| p[0].t < p[1].t));
    }

    #[test]
    fn cross_store_equal_priorities_keep_previous_tie_behavior() {
        for flags in [0, pt_flags::MLAT] {
            for offset in [0.0, 0.03125] {
                for reverse_stores in [false, true] {
                    let first = merge_test_point(0.0, flags);
                    let mut second = merge_test_point(offset, flags);
                    second.alt_m += 50.0;
                    let a = merge_test_store(vec![first, merge_test_point(10.0, flags)], "a");
                    let b = merge_test_store(vec![second, merge_test_point(20.0, flags)], "b");
                    let stores = if reverse_stores {
                        vec![b, a]
                    } else {
                        vec![a, b]
                    };
                    let merged = merge_stores(stores, 300.0);
                    let expected = if offset == 0.0 && reverse_stores {
                        second
                    } else {
                        first
                    };
                    assert_eq!(merged.legs[0].points[0], expected);
                    assert_eq!(merged.legs[0].points.len(), 3);
                }
            }
        }
    }

    #[test]
    fn cross_store_equal_priorities_preserve_three_store_clusters() {
        for flags in [0, pt_flags::MLAT] {
            let p = |t| merge_test_point(t, flags);
            let merged = merge_stores(
                vec![
                    merge_test_store(vec![p(-10.0), p(0.0), p(0.09375), p(10.0)], "a"),
                    merge_test_store(vec![p(-5.0), p(0.03125), p(10.0)], "b"),
                    merge_test_store(vec![p(-1.0), p(-0.03125), p(20.0)], "c"),
                ],
                300.0,
            );
            // B's 0.03125 was discarded with A's 0.0 before C shifted the anchor.
            // No quality improved, so final selection must not resurrect that sample.
            assert_eq!(merged.legs.len(), 1);
            assert_eq!(
                merged.legs[0].points,
                [
                    p(-10.0),
                    p(-5.0),
                    p(-1.0),
                    p(-0.03125),
                    p(0.09375),
                    p(10.0),
                    p(20.0),
                ]
            );
            assert_eq!(merged.meta.stats["cross_store_joins"], 2);
        }
    }

    #[test]
    fn cross_store_equal_priorities_preserve_reintroduced_payload() {
        for flags in [0, pt_flags::MLAT] {
            let p = |t| merge_test_point(t, flags);
            let mut discarded = p(0.03125);
            discarded.alt_m = 1100.0;
            let mut retained = p(0.03125);
            retained.alt_m = 1300.0;
            let merged = merge_stores(
                vec![
                    merge_test_store(vec![p(-10.0), p(0.0), p(10.0)], "a"),
                    merge_test_store(vec![p(-5.0), discarded, p(10.0)], "b"),
                    merge_test_store(vec![p(-1.0), p(-0.03125), retained, p(20.0)], "c"),
                ],
                300.0,
            );
            // C reintroduces this timestamp after B's report was already discarded.
            // A same-quality raw report cannot replace the original surviving payload.
            assert_eq!(merged.legs.len(), 1);
            assert_eq!(
                merged.legs[0].points,
                [
                    p(-10.0),
                    p(-5.0),
                    p(-1.0),
                    p(-0.03125),
                    retained,
                    p(10.0),
                    p(20.0),
                ]
            );
            assert_eq!(merged.meta.stats["cross_store_joins"], 2);
        }
    }

    #[test]
    fn cross_store_priority_does_not_join_same_store_legs() {
        let p = merge_test_point;
        let mut a = merge_test_store(vec![p(0.0, 0), p(10.0, pt_flags::MLAT)], "a");
        let returning = merge_test_store(vec![p(30.0, 0), p(40.0, 0)], "return");
        a.meta.t_end = returning.meta.t_end;
        a.legs.extend(returning.legs);
        let b = merge_test_store(vec![p(10.03125, 0), p(20.0, 0)], "b");
        let merged = merge_stores(vec![a, b], 300.0);
        assert_eq!(merged.legs.len(), 2);
        assert_eq!(
            merged.legs[0].points,
            [p(0.0, 0), p(10.03125, 0), p(20.0, 0)]
        );
        assert_eq!(merged.legs[1].points, [p(30.0, 0), p(40.0, 0)]);
        assert_eq!(merged.legs[0].key, "abc123_19700101_0");
        assert_eq!(merged.legs[1].key, "abc123_19700101_1");
        assert_eq!(merged.meta.stats["cross_store_joins"], 1);
    }
}
