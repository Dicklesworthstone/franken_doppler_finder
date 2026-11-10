//! Fetch the tracks for a search window: per UTC day, use a cached day store whose bbox
//! covers the request, otherwise stream the day's adsb.lol release(s) and cache the result.
//!
//! Cache layout: `<cache>/adsb/<YYYY-MM-DD>/<feedset>_<S>_<W>_<N>_<E>.fdtrk` (bbox rounded
//! outward to 0.1 degree; the whole UTC day is kept, the window is applied at load) and
//! `<cache>/adsb/<YYYY-MM-DD>/releases.json`. Day stores are always built with unknown types
//! kept; `keep_unknown = false` is applied after cross-day metadata reconciliation.

use crate::DataError;
use crate::adsblol::{DayRelease, Feed, find_day_releases_cached};
use crate::build::{build_day_store, merge_stores};
use crate::ingest::{
    DayContext, IngestProgress, IngestSpec, IngestStats, RawAircraft, ingest_day_reader,
};
use fdop_types::BBox;
use fdop_types::engine::EngineClass;
use fdop_types::store::{FlightLeg, StoreMeta, TrackStore, alt_src};
use fdop_types::time::{civil_from_days, days_from_civil};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Seek;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Legs of one aircraft in consecutive day stores closer than this are joined (s).
pub const DAY_JOIN_GAP_S: f64 = 300.0;
const DAY_CACHE_PAD_S: f64 = 3600.0;

fn day_context(day: (i64, u32, u32)) -> DayContext {
    DayContext::new(
        days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0,
        DAY_JOIN_GAP_S,
        DAY_CACHE_PAD_S,
    )
}

pub type ProgressFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct FetchOptions {
    /// Also ingest the MLAT-only release of each day (recommended: adds aircraft without ADS-B out).
    pub include_mlat: bool,
    /// Concurrent release streams.
    pub parallel: usize,
    /// Never touch the network: use cached day stores only.
    pub offline: bool,
    /// Skip days without a release (or without a cache when offline) instead of failing.
    pub allow_missing: bool,
    /// Receives human-readable progress lines (roughly every 5 s per stream).
    pub progress: Option<ProgressFn>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            include_mlat: true,
            parallel: 3,
            offline: false,
            allow_missing: false,
            progress: None,
        }
    }
}

impl FetchOptions {
    fn say(&self, s: &str) {
        if let Some(p) = &self.progress {
            p(s);
        }
    }
}

/// UTC days (y, m, d) overlapping `[t0, t1]`.
pub fn utc_days(t0: f64, t1: f64) -> Vec<(i64, u32, u32)> {
    let a = (t0 / 86_400.0).floor() as i64;
    let b = (t1 / 86_400.0).floor() as i64;
    (a..=b.max(a)).map(civil_from_days).collect()
}

/// Bbox rounded outward to 0.1 degree, and its cache-key string `S_W_N_E`.
pub fn bbox_key(b: &BBox) -> (BBox, String) {
    let r = BBox::new(
        (b.south * 10.0 + 1e-6).floor() / 10.0,
        (b.west * 10.0 + 1e-6).floor() / 10.0,
        (b.north * 10.0 - 1e-6).ceil() / 10.0,
        (b.east * 10.0 - 1e-6).ceil() / 10.0,
    );
    let key = format!("{:.1}_{:.1}_{:.1}_{:.1}", r.south, r.west, r.north, r.east);
    (r, key)
}

fn covers(outer: &BBox, inner: &BBox) -> bool {
    outer.south <= inner.south + 1e-9
        && outer.west <= inner.west + 1e-9
        && outer.north >= inner.north - 1e-9
        && outer.east >= inner.east - 1e-9
}

/// Parse `<feedset>_<S>_<W>_<N>_<E>.fdtrk`.
pub fn parse_cache_name(name: &str) -> Option<(String, BBox)> {
    let stem = name.strip_suffix(".fdtrk")?;
    let (feedset, rest) = stem.split_once('_')?;
    let v: Vec<f64> = rest
        .split('_')
        .map(|x| x.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    (v.len() == 4).then(|| (feedset.to_string(), BBox::new(v[0], v[1], v[2], v[3])))
}

fn day_dir(cache: &Path, (y, m, d): (i64, u32, u32)) -> PathBuf {
    cache.join("adsb").join(format!("{y:04}-{m:02}-{d:02}"))
}

/// A conservative envelope of every report for one aircraft. Whole-aircraft selection
/// preserves leg joins and key counters, including legs outside the final requested slice.
#[derive(Clone, Copy)]
struct AircraftBounds {
    t0: f64,
    t1: f64,
    bbox: BBox,
    uncertain: bool,
}

impl Default for AircraftBounds {
    fn default() -> Self {
        Self {
            t0: f64::INFINITY,
            t1: f64::NEG_INFINITY,
            bbox: BBox::new(
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            uncertain: false,
        }
    }
}

impl AircraftBounds {
    fn extend(&mut self, other: &Self) {
        self.t0 = self.t0.min(other.t0);
        self.t1 = self.t1.max(other.t1);
        self.bbox = self.bbox.union(&other.bbox);
        self.uncertain |= other.uncertain;
    }

    fn overlaps(&self, spec: &IngestSpec) -> bool {
        self.uncertain
            || (bounds_overlap_axis(self.t0, self.t1, spec.t_start, spec.t_end)
                && bounds_overlap_axis(
                    self.bbox.south,
                    self.bbox.north,
                    spec.bbox.south,
                    spec.bbox.north,
                )
                // A short segment can wrap across the dateline instead of taking the
                // ordinary longitude interval. Keep that aircraft at any longitude.
                && (self.bbox.east - self.bbox.west > 180.0
                    || [-360.0, 0.0, 360.0].into_iter().any(|shift| {
                        bounds_overlap_axis(
                            self.bbox.west,
                            self.bbox.east,
                            spec.bbox.west + shift,
                            spec.bbox.east + shift,
                        )
                    })))
    }
}

/// A conservative prefilter for the existing floating-point segment clip. Its endpoint
/// subtraction and division can round an intersection onto an endpoint just outside the
/// exact envelope. Expand only this prefilter by a few arithmetic ulps; the final filter
/// remains authoritative. Include both intervals' magnitudes (and shifted longitudes).
fn bounds_overlap_axis(lo: f64, hi: f64, query_lo: f64, query_hi: f64) -> bool {
    if lo > hi {
        return false;
    }
    let scale = lo
        .abs()
        .max(hi.abs())
        .max(query_lo.abs())
        .max(query_hi.abs())
        .max(1.0);
    let roundoff = 8.0 * f64::EPSILON * scale;
    lo - roundoff <= query_hi && hi + roundoff >= query_lo
}

fn include_leg_bounds(bounds: &mut HashMap<String, AircraftBounds>, leg: &FlightLeg) {
    let b = bounds.entry(leg.icao.clone()).or_default();
    for p in &leg.points {
        if ![p.t, p.lat, p.lon].into_iter().all(f64::is_finite) {
            b.uncertain = true;
            continue;
        }
        b.t0 = b.t0.min(p.t);
        b.t1 = b.t1.max(p.t);
        b.bbox.south = b.bbox.south.min(p.lat);
        b.bbox.north = b.bbox.north.max(p.lat);
        b.bbox.west = b.bbox.west.min(p.lon);
        b.bbox.east = b.bbox.east.max(p.lon);
    }
}

struct CachedDay {
    file: File,
    path: PathBuf,
    meta: StoreMeta,
    leg_count: usize,
    bounds: HashMap<String, AircraftBounds>,
}

impl CachedDay {
    /// Summarize a freshly built day before releasing its point arrays. Saving returns
    /// the actual published inode, so a concurrent replacement cannot change the data
    /// behind these bounds. The later selected-aircraft read checks the completed gzip.
    fn publish(store: TrackStore, path: &Path) -> std::io::Result<Self> {
        let mut bounds = HashMap::new();
        for leg in &store.legs {
            include_leg_bounds(&mut bounds, leg);
        }
        let file = store.save_open(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            leg_count: store.legs.len(),
            meta: store.meta,
            bounds,
        })
    }

    fn scan(path: &Path) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let mut bounds = HashMap::new();
        let mut leg_count = 0;
        let meta = TrackStore::read_legs(&mut file, path, |leg| {
            leg_count += 1;
            include_leg_bounds(&mut bounds, &leg);
        })?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            meta,
            leg_count,
            bounds,
        })
    }

    fn load_selected(mut self, selected: &HashSet<String>) -> std::io::Result<TrackStore> {
        let needed = self.bounds.keys().any(|id| selected.contains(id));
        drop(self.bounds);
        let mut legs = Vec::new();
        if needed {
            self.file.rewind()?;
            TrackStore::read_legs(&mut self.file, &self.path, |leg| {
                if selected.contains(&leg.icao) {
                    legs.push(leg);
                }
            })?;
        }
        // Keep even empty stores and their original metadata: store indices, source
        // attribution, per-day statistics and altitude references must not change.
        Ok(TrackStore {
            meta: self.meta,
            legs,
        })
    }
}

/// Combine all day summaries before selecting aircraft: a new cross-day segment can
/// cross the requested box even though neither day's separate leg does. Retain all of
/// each chosen aircraft, then leave the existing merge and final filter authoritative.
fn load_selected_stores(
    stores: Vec<CachedDay>,
    spec: &IngestSpec,
) -> Result<Vec<TrackStore>, DataError> {
    let mut bounds: HashMap<String, AircraftBounds> = HashMap::new();
    for store in &stores {
        for (id, b) in &store.bounds {
            bounds.entry(id.clone()).or_default().extend(b);
        }
    }
    let selected: HashSet<String> = bounds
        .into_iter()
        .filter_map(|(id, b)| b.overlaps(spec).then_some(id))
        .collect();
    stores
        .into_iter()
        .map(|store| store.load_selected(&selected).map_err(DataError::from))
        .collect()
}

/// Smallest readable cached day store for `feedset` covering the requested day slice.
fn find_cached(
    dir: &Path,
    feedset: &str,
    want: &IngestSpec,
    label: &str,
    opts: &FetchOptions,
    required_context: Option<DayContext>,
) -> Option<(PathBuf, CachedDay)> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut candidates = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some((fs, bb)) = parse_cache_name(&name)
            && fs == feedset
            && covers(&bb, &want.bbox)
        {
            candidates.push((bb.area_km2(), e.path()));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut legacy = None;
    for (_, p) in candidates {
        match CachedDay::scan(&p) {
            Ok(s)
                if covers(&s.meta.bbox, &want.bbox)
                    && s.meta.t_start <= want.t_start
                    && s.meta.t_end >= want.t_end =>
            {
                if required_context.is_some_and(|required|
                    !DayContext::from_stats(&s.meta.stats).is_some_and(|context| context.includes(required)))
                {
                    if opts.offline {
                        if legacy.is_none() {
                            legacy = Some((p, s));
                        }
                    } else {
                        opts.say(&format!("[adsb] {label}: cache {} lacks verified midnight coverage; rebuilding for this multi-day window", p.display()));
                    }
                    continue;
                }
                return Some((p, s));
            }
            Ok(_) => opts.say(&format!(
                "[adsb] {label}: cache {} does not cover the requested region and time window; checking other cached stores",
                p.display()
            )),
            Err(e) => opts.say(&format!(
                "[adsb] {label}: unreadable cache {} ({e}); checking other cached stores",
                p.display()
            )),
        }
    }
    legacy
}

fn cache_context_note(
    label: &str,
    store: &CachedDay,
    required: Option<DayContext>,
) -> Option<String> {
    required.filter(|required| !DayContext::from_stats(&store.meta.stats)
        .is_some_and(|context| context.includes(*required)))
        .map(|_| format!("{label}: legacy day cache lacks verified midnight coverage; cross-midnight flights may be missing or discontinuous; rerun online to rebuild this day"))
}

/// Cache feedset name of a day store built from `main` (prod or staging) plus optional mlatonly.
fn feedset_name(main: Feed, mlat: bool) -> String {
    format!("{}{}", main.tag_word(), if mlat { "+mlatonly" } else { "" })
}

/// Feedset of completed release tags. MLAT alone cannot stand in for a main feed.
fn feedset_of_tags(tags: &[String]) -> Option<String> {
    let has = |f: Feed| {
        tags.iter()
            .any(|t| t.contains(&format!("-{}-", f.tag_word())))
    };
    let main = if has(Feed::Prod) {
        Feed::Prod
    } else if has(Feed::Staging) {
        Feed::Staging
    } else {
        return None;
    };
    Some(feedset_name(main, has(Feed::Mlatonly)))
}

/// Best feedset obtainable from `rels` (None without a prod or staging release).
fn feedset_available(rels: &[DayRelease], include_mlat: bool) -> Option<String> {
    let main = if rels.iter().any(|r| r.feed == Feed::Prod) {
        Feed::Prod
    } else if rels.iter().any(|r| r.feed == Feed::Staging) {
        Feed::Staging
    } else {
        return None;
    };
    Some(feedset_name(
        main,
        include_mlat && rels.iter().any(|r| r.feed == Feed::Mlatonly),
    ))
}

const NO_MLATONLY: &str = "no mlatonly data in this catalogue, so aircraft located only by multilateration \
                           (transponders without ADS-B Out) are missing";

/// What a day built from feedset `fs` lacks, for warnings (`label` is the day).
fn feedset_gaps(label: &str, fs: &str, include_mlat: bool) -> Vec<String> {
    let mut gaps = Vec::new();
    if fs.starts_with("staging") {
        gaps.push(format!("{label}: prod release missing, using staging"));
    }
    if include_mlat && !fs.contains("mlatonly") {
        gaps.push(format!("{label}: {NO_MLATONLY}"));
    }
    gaps
}

/// Lesser feedsets that can stand in for `want` when the day lacks a release.
fn partial_feedsets(want: &str) -> &'static [&'static str] {
    if want == "prod+mlatonly" {
        &["prod", "staging+mlatonly", "staging"]
    } else {
        &["staging"]
    }
}

/// adsb.lol publishes a UTC day's archive some hours after the day ends; a missing day younger
/// than this may still appear, an older one never will.
pub const PUBLISH_LAG_S: f64 = 2.0 * 86_400.0;

/// End of the usable ADS-B archive for `[t0, t1]` at time `now`, and the labels of the trailing
/// UTC days that are pending: not over yet, or over for less than [`PUBLISH_LAG_S`] with neither
/// a cached day store nor a prod/staging release (offline: no cached store). Returns `(t1, [])`
/// when the last day is usable. Older missing days are not pending; `fetch_window` reports them.
pub fn published_until(
    t0: f64,
    t1: f64,
    cache_dir: &Path,
    offline: bool,
    now: f64,
) -> Result<(f64, Vec<String>), DataError> {
    let mut until = t1;
    let mut pending = Vec::new();
    let mut net: Option<fdop_http::Net> = None;
    for day in utc_days(t0, t1).into_iter().rev() {
        let start = days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0;
        let end = start + 86_400.0;
        let label = format!("{:04}-{:02}-{:02}", day.0, day.1, day.2);
        let is_pending = if end > now {
            true
        } else if end + PUBLISH_LAG_S > now {
            let dir = day_dir(cache_dir, day);
            let cached = std::fs::read_dir(&dir).is_ok_and(|rd| {
                rd.flatten()
                    .any(|e| parse_cache_name(&e.file_name().to_string_lossy()).is_some())
            });
            if cached {
                false
            } else if offline {
                true
            } else {
                if net.is_none() {
                    net = Some(fdop_http::Net::new()?);
                }
                let rels =
                    find_day_releases_cached(net.as_ref(), day.0, day.1, day.2, &dir, false)?;
                !rels
                    .iter()
                    .any(|r| matches!(r.feed, Feed::Prod | Feed::Staging))
            }
        } else {
            false
        };
        if !is_pending {
            break;
        }
        until = start;
        pending.push(label);
    }
    pending.reverse();
    Ok((until.min(t1), pending))
}

/// Re-reference GNSS-derived heights from the store's geoid value to `n_target`.
pub fn adjust_geoid(store: &mut TrackStore, n_target: f64) {
    let d = (store.meta.geoid_n_m - n_target) as f32;
    if d.abs() < 0.01 {
        return;
    }
    for leg in &mut store.legs {
        for p in &mut leg.points {
            if p.alt_src != alt_src::BARO_RAW {
                p.alt_m += d;
            }
        }
    }
    store.meta.geoid_n_m = n_target;
}

fn mib(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

struct Job {
    day_idx: usize,
    day: (i64, u32, u32),
    rel: DayRelease,
}

type Part = (Vec<RawAircraft>, IngestStats);
type JobResult = (usize, String, Result<Part, DataError>);

#[derive(Debug, Default)]
struct StreamedDayFailures {
    feeds: Vec<(usize, String)>,
    caches: Vec<(usize, String)>,
}

impl StreamedDayFailures {
    fn report(&self, opts: &FetchOptions, notes: &mut Vec<String>) -> Result<(), DataError> {
        if self.feeds.is_empty() && self.caches.is_empty() {
            return Ok(());
        }
        let msg = format!(
            "ADS-B fetch failed (rerun to retry incomplete feeds or caches): {}",
            self.feeds
                .iter()
                .chain(&self.caches)
                .map(|(_, e)| e.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        // Strict searches still fail on a missing feed. A cache write failure remains
        // fatal even when the caller explicitly allows incomplete archive coverage.
        if !opts.allow_missing || !self.caches.is_empty() {
            return Err(DataError(msg));
        }
        opts.say(&format!("[adsb] WARNING {msg}"));
        notes.push(msg);
        Ok(())
    }
}

/// Publish completed feeds after every requested feed for the day has finished. Preserve
/// a successful main feed when MLAT fails, but never publish an MLAT-only remainder as a
/// main catalogue. The caller drains a bounded channel while workers are still running.
fn collect_streamed_days(
    results: impl IntoIterator<Item = JobResult>,
    mut remaining: Vec<usize>,
    mut finish: impl FnMut(usize, Vec<Part>, Vec<String>, String) -> Result<(), DataError>,
) -> StreamedDayFailures {
    let mut pending: BTreeMap<usize, (Vec<Part>, Vec<String>)> = BTreeMap::new();
    let mut failures = StreamedDayFailures::default();
    for (di, tag, result) in results {
        remaining[di] -= 1;
        match result {
            Ok(part) => {
                let (parts, tags) = pending.entry(di).or_default();
                parts.push(part);
                tags.push(tag);
            }
            Err(e) => failures.feeds.push((di, e.0)),
        }
        if remaining[di] == 0
            && let Some((parts, mut tags)) = pending.remove(&di)
            && let Some(feedset) = feedset_of_tags(&tags)
        {
            tags.sort();
            if let Err(e) = finish(di, parts, tags, feedset) {
                failures.caches.push((di, e.0));
            }
        }
    }
    failures
}

/// Tracks for `spec` (bbox + window), downloading and caching whole UTC days as needed.
pub fn fetch_window(
    spec: &IngestSpec,
    cache_dir: &Path,
    opts: &FetchOptions,
) -> Result<TrackStore, DataError> {
    let feedset = if opts.include_mlat {
        "prod+mlatonly"
    } else {
        "prod"
    };
    let (rbox, key) = bbox_key(&spec.bbox);
    let days = utc_days(spec.t_start, spec.t_end);
    let mut day_stores: Vec<Option<CachedDay>> = (0..days.len()).map(|_| None).collect();
    let mut notes: Vec<String> = Vec::new();
    let mut cache_hits = 0usize;
    let mut jobs: Vec<Job> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let net = if opts.offline {
        None
    } else {
        Some(fdop_http::Net::new()?)
    };
    for (i, &day) in days.iter().enumerate() {
        let dir = day_dir(cache_dir, day);
        let label = format!("{:04}-{:02}-{:02}", day.0, day.1, day.2);
        let day_start = days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0;
        let want = IngestSpec {
            t_start: spec.t_start.max(day_start),
            t_end: spec.t_end.min(day_start + 86_400.0),
            ..*spec
        };
        let required_context = (days.len() > 1).then(|| day_context(day));
        if let Some((p, s)) = find_cached(&dir, feedset, &want, &label, opts, required_context) {
            opts.say(&format!(
                "[adsb] {label}: cached {} ({} legs)",
                p.display(),
                s.leg_count
            ));
            notes.extend(cache_context_note(&label, &s, required_context));
            day_stores[i] = Some(s);
            cache_hits += 1;
            continue;
        }
        // a store built while a release was missing: use it when nothing better exists
        let rels = if opts.offline {
            Vec::new()
        } else {
            find_day_releases_cached(net.as_ref(), day.0, day.1, day.2, &dir, false)?
        };
        let available = feedset_available(&rels, opts.include_mlat);
        for fs in partial_feedsets(feedset) {
            if (opts.offline || available.as_ref().is_none_or(|a| a.as_str() == *fs))
                && let Some((p, s)) = find_cached(&dir, fs, &want, &label, opts, required_context)
            {
                opts.say(&format!(
                    "[adsb] {label}: cached {} ({} legs; {fs} only)",
                    p.display(),
                    s.leg_count
                ));
                notes.extend(feedset_gaps(&label, fs, opts.include_mlat));
                notes.extend(cache_context_note(&label, &s, required_context));
                day_stores[i] = Some(s);
                cache_hits += 1;
                break;
            }
        }
        if day_stores[i].is_some() {
            continue;
        }
        if opts.offline {
            missing.push(format!("{label} (offline, not cached)"));
            continue;
        }
        let main = rels
            .iter()
            .find(|r| r.feed == Feed::Prod)
            .or_else(|| rels.iter().find(|r| r.feed == Feed::Staging))
            .cloned();
        let Some(main) = main else {
            missing.push(format!(
                "{label} (no prod/staging release in {})",
                crate::adsblol::repo_for_year(day.0)
            ));
            continue;
        };
        if main.feed == Feed::Staging {
            notes.push(format!(
                "{label}: prod release missing, using staging {}",
                main.tag
            ));
        }
        jobs.push(Job {
            day_idx: i,
            day,
            rel: main,
        });
        if opts.include_mlat {
            match rels.iter().find(|r| r.feed == Feed::Mlatonly) {
                Some(r) => jobs.push(Job {
                    day_idx: i,
                    day,
                    rel: r.clone(),
                }),
                None => notes.push(format!("{label}: {NO_MLATONLY}")),
            }
        }
    }
    if !missing.is_empty() {
        let msg = format!("no ADS-B data for UTC day(s): {}", missing.join("; "));
        if !opts.allow_missing {
            return Err(DataError(msg));
        }
        opts.say(&format!("[adsb] WARNING {msg}"));
        notes.push(msg);
    }
    for n in &notes {
        opts.say(&format!("[adsb] {n}"));
    }

    // ---- stream the uncached releases ----
    let day_spec = |day: (i64, u32, u32)| IngestSpec {
        bbox: rbox,
        t_start: days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0 - DAY_CACHE_PAD_S,
        t_end: days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0 + 86_400.0 + DAY_CACHE_PAD_S,
        geoid_n_m: spec.geoid_n_m,
        keep_unknown: true,
    };
    let mut failed = StreamedDayFailures::default();
    if !jobs.is_empty() {
        let total: u64 = jobs.iter().map(|j| j.rel.total_bytes()).sum();
        opts.say(&format!(
            "[adsb] streaming {} release(s), {:.0} MiB, {} at a time",
            jobs.len(),
            mib(total),
            opts.parallel.max(1)
        ));
        let mut remaining = vec![0usize; days.len()];
        for job in &jobs {
            remaining[job.day_idx] += 1;
        }
        let workers = opts.parallel.max(1).min(jobs.len());
        let queue = Mutex::new(jobs.into_iter().collect::<VecDeque<Job>>());
        // Backpressure bounds completed raw releases while one day is built and saved.
        // Receiving must happen inside the scope, before joining the producers.
        let (tx, rx) = std::sync::mpsc::sync_channel::<JobResult>(workers);
        failed = std::thread::scope(|s| {
            for _ in 0..workers {
                let tx = tx.clone();
                let queue = &queue;
                let day_spec = &day_spec;
                s.spawn(move || {
                    loop {
                        let job = match queue.lock() {
                            Ok(mut q) => q.pop_front(),
                            Err(_) => None,
                        };
                        let Some(job) = job else { break };
                        let label = format!("{:04}-{:02}-{:02} {}", job.day.0, job.day.1, job.day.2, job.rel.feed.tag_word());
                        let size = job.rel.total_bytes();
                        let reader = crate::stream::BigStreamReader::open(job.rel.urls());
                        let sp = day_spec(job.day);
                        let cb = |p: &IngestProgress| {
                            let rate = if p.elapsed_s > 0.0 { mib(p.bytes) / p.elapsed_s } else { 0.0 };
                            opts.say(&format!(
                                "[adsb] {label}: {:.0}/{:.0} MiB ({:.0}%) {:.1} MiB/s, {} members, {} kept, {:.0}s",
                                mib(p.bytes),
                                mib(size),
                                100.0 * p.bytes as f64 / size.max(1) as f64,
                                rate,
                                p.members,
                                p.kept_aircraft,
                                p.elapsed_s
                            ));
                        };
                        let res = ingest_day_reader(reader, job.rel.feed.label(), &sp, &cb, day_context(job.day))
                            .map_err(|e| DataError(format!("{label} ({}): {e}", job.rel.tag)));
                        if tx.send((job.day_idx, job.rel.tag.clone(), res)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            collect_streamed_days(rx, remaining, |di, parts, tags, fs| {
                let day = days[di];
                // Only completely ingested feeds reach this cache. Its actual feedset
                // lets offline runs use a surviving main feed with the existing warning;
                // an online retry rebuilds the requested feeds together for altitude/type
                // reconciliation instead of merging independently built day stores.
                if opts.include_mlat && !fs.contains("mlatonly") {
                    let label = format!("{:04}-{:02}-{:02}", day.0, day.1, day.2);
                    let note = format!("{label}: {NO_MLATONLY}");
                    if !notes.contains(&note) {
                        notes.push(note);
                    }
                }
                let store = build_day_store(parts, &day_spec(day), tags, day_context(day));
                let dir = day_dir(cache_dir, day);
                let path = dir.join(format!("{fs}_{key}.fdtrk"));
                let (legs, points) = (store.legs.len(), store.n_points());
                let published = CachedDay::publish(store, &path).map_err(|e| {
                    DataError(format!(
                        "could not cache completed day {}: {e}",
                        path.display()
                    ))
                })?;
                opts.say(&format!(
                    "[adsb] {:04}-{:02}-{:02}: built {} legs / {} points -> {}",
                    day.0,
                    day.1,
                    day.2,
                    legs,
                    points,
                    path.display()
                ));
                day_stores[di] = Some(published);
                Ok(())
            })
        });
    }

    failed.report(opts, &mut notes)?;

    // ---- combine ----
    let stores: Vec<CachedDay> = day_stores.into_iter().flatten().collect();
    if stores.is_empty() {
        return Err(DataError(
            "no ADS-B day stores available for the window".into(),
        ));
    }
    let mut stores = load_selected_stores(stores, spec)?;
    let mut day_stats = Vec::new();
    for s in &mut stores {
        adjust_geoid(s, spec.geoid_n_m);
        day_stats.push(serde_json::json!({"sources": s.meta.sources, "stats": s.meta.stats}));
    }
    let mut merged = merge_stores(stores, DAY_JOIN_GAP_S);
    // An adjoining day's type can identify the whole flight. Dropping its unknown
    // half before the merge would discard geometry that now has a known type.
    if !spec.keep_unknown {
        merged.legs.retain(|l| l.engine != EngineClass::Unknown);
    }
    let mut out = merged.filtered(&spec.bbox, spec.t_start, spec.t_end);
    out.meta.geoid_n_m = spec.geoid_n_m;
    out.meta.stats = serde_json::json!({
        "legs": out.legs.len(),
        "points": out.n_points(),
        "days": days.iter().map(|d| format!("{:04}-{:02}-{:02}", d.0, d.1, d.2)).collect::<Vec<_>>(),
        "cache_hits": cache_hits,
        "feedset": feedset,
        "notes": notes,
        "day_stores": day_stats,
        "leg_boundaries": out.meta.stats.get("leg_boundaries").cloned().unwrap_or_default(),
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::build_store;
    use crate::ingest::ingest_reader;

    #[test]
    fn every_stand_in_feedset_is_reported() {
        for want in ["prod+mlatonly", "prod"] {
            let mlat = want.contains("mlatonly");
            for fs in partial_feedsets(want) {
                assert!(!feedset_gaps("d", fs, mlat).is_empty(), "{want} <- {fs}");
            }
        }
    }

    #[test]
    fn keys_days_and_names() {
        let b = BBox::new(40.2324, -80.1176, 45.2847, -71.4744);
        let (r, k) = bbox_key(&b);
        assert_eq!(k, "40.2_-80.2_45.3_-71.4");
        assert!(covers(&r, &b));
        let (fs, bb) = parse_cache_name(&format!("prod+mlatonly_{k}.fdtrk")).unwrap();
        assert_eq!(fs, "prod+mlatonly");
        assert_eq!(bb, r);
        assert!(parse_cache_name("prod_1_2_3.fdtrk").is_none());
        let t0 = fdop_types::time::parse_iso_utc("2026-09-08T04:00:00Z").unwrap();
        let t1 = fdop_types::time::parse_iso_utc("2026-09-10T11:00:00Z").unwrap();
        assert_eq!(
            utc_days(t0, t1),
            vec![(2026, 9, 8), (2026, 9, 9), (2026, 9, 10)]
        );
        // exact-tenth inputs do not grow
        assert_eq!(
            bbox_key(&BBox::new(40.0, -74.0, 41.0, -73.0)).1,
            "40.0_-74.0_41.0_-73.0"
        );
    }

    fn synthetic_part(sp: &IngestSpec, feed: &str) -> Part {
        use crate::ingest::tests::{gz, pc12_json, tar_of};
        let tarb = tar_of(&[("./traces/c3/trace_full_a1b2c3.json", gz(&pc12_json()))]);
        ingest_reader(std::io::Cursor::new(tarb), feed, sp, &|_| {}).unwrap()
    }

    fn synthetic_store(sp: &IngestSpec) -> TrackStore {
        build_store(
            vec![synthetic_part(sp, "prod")],
            sp,
            vec!["synthetic".into()],
        )
    }

    #[test]
    fn midnight_cache_policy_preserves_single_day_reuse_and_prefers_proven_coverage() {
        use crate::ingest::tests::{T0, gz, pc12_json, spec, tar_of};
        let want = spec();
        let day = (2026, 9, 8);
        let context = day_context(day);
        let label = "2026-09-08";
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = std::env::temp_dir().join(format!(
            "fdop-midnight-cache-policy-{}-{nonce}",
            std::process::id()
        ));
        let dir = day_dir(&cache, day);
        let (small_box, small_key) = bbox_key(&want.bbox);
        let cache_spec = IngestSpec {
            bbox: small_box,
            t_start: T0 - DAY_CACHE_PAD_S,
            t_end: T0 + 86_400.0 + DAY_CACHE_PAD_S,
            ..want
        };
        let legacy_path = dir.join(format!("prod_{small_key}.fdtrk"));
        synthetic_store(&cache_spec).save(&legacy_path).unwrap();
        let offline = FetchOptions {
            include_mlat: false,
            offline: true,
            ..Default::default()
        };
        let online = FetchOptions {
            offline: false,
            ..offline.clone()
        };
        for opts in [&online, &offline] {
            let (path, store) = find_cached(&dir, "prod", &want, label, opts, None).unwrap();
            assert_eq!(path, legacy_path, "single-day legacy reuse changed");
            assert!(cache_context_note(label, &store, None).is_none());
        }
        // This exercises the actual offline fetch, not just its cache selector.
        let single_day = fetch_window(&want, &cache, &offline).unwrap();
        assert_eq!(single_day.meta.stats["cache_hits"], 1);
        assert!(single_day.n_points() > 0);
        assert_eq!(single_day.meta.stats["notes"], serde_json::json!([]));
        // The selector itself has no network path; online policy must decline this
        // legacy cache before fetch_window decides which archive to download.
        assert!(find_cached(&dir, "prod", &want, label, &online, Some(context)).is_none());
        let (path, legacy) =
            find_cached(&dir, "prod", &want, label, &offline, Some(context)).unwrap();
        assert_eq!(path, legacy_path);
        let note = cache_context_note(label, &legacy, Some(context)).unwrap();
        assert!(note.starts_with("2026-09-08: legacy day cache lacks verified midnight coverage"));
        assert!(note.contains("cross-midnight flights may be missing or discontinuous"));
        assert!(note.contains("rerun online"));

        let (large_box, large_key) = bbox_key(&small_box.padded_km(20.0));
        assert!(large_box.area_km2() > small_box.area_km2());
        let large_spec = IngestSpec {
            bbox: large_box,
            ..cache_spec
        };
        let archive = tar_of(&[("trace_full_a1b2c3.json", gz(&pc12_json()))]);
        let part =
            ingest_day_reader(archive.as_slice(), "prod", &large_spec, &|_| {}, context).unwrap();
        let marked = build_day_store(vec![part], &large_spec, vec!["synthetic".into()], context);
        let marked_path = dir.join(format!("prod_{large_key}.fdtrk"));
        marked.save(&marked_path).unwrap();
        for opts in [&online, &offline] {
            let (path, store) =
                find_cached(&dir, "prod", &want, label, opts, Some(context)).unwrap();
            assert_eq!(
                path, marked_path,
                "a smaller legacy cache hid proven coverage"
            );
            assert!(cache_context_note(label, &store, Some(context)).is_none());
        }
        // A marker for another day or an unknown policy version is not coverage proof.
        assert!(
            find_cached(
                &dir,
                "prod",
                &want,
                label,
                &online,
                Some(day_context((2026, 9, 9)))
            )
            .is_none()
        );
        let mut future = marked;
        future.meta.stats["midnight_context"]["version"] = serde_json::json!(255);
        future.save(&marked_path).unwrap();
        assert!(find_cached(&dir, "prod", &want, label, &online, Some(context)).is_none());
        let (path, _) = find_cached(&dir, "prod", &want, label, &offline, Some(context)).unwrap();
        assert_eq!(
            path, legacy_path,
            "offline fallback must keep the smallest usable legacy cache"
        );
    }

    fn synthetic_tag(feed: Feed) -> String {
        crate::adsblol::tag_for(2026, 9, 8, feed, 0)
    }

    #[test]
    fn completed_feedset_requires_a_main_release() {
        let prod = synthetic_tag(Feed::Prod);
        let staging = synthetic_tag(Feed::Staging);
        let mlat = synthetic_tag(Feed::Mlatonly);
        assert_eq!(feedset_of_tags(&[]), None);
        assert_eq!(feedset_of_tags(std::slice::from_ref(&mlat)), None);
        assert_eq!(feedset_of_tags(&["mlat".into()]), None);
        assert_eq!(
            feedset_of_tags(std::slice::from_ref(&prod)).as_deref(),
            Some("prod")
        );
        assert_eq!(
            feedset_of_tags(std::slice::from_ref(&staging)).as_deref(),
            Some("staging")
        );
        assert_eq!(
            feedset_of_tags(&[prod, mlat.clone()]).as_deref(),
            Some("prod+mlatonly")
        );
        assert_eq!(
            feedset_of_tags(&[staging, mlat]).as_deref(),
            Some("staging+mlatonly")
        );
    }

    #[test]
    fn streamed_days_are_cached_before_unrelated_downloads_finish() {
        use crate::ingest::tests::spec;
        let sp = spec();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("fdop-streamed-days-{}-{nonce}", std::process::id()));
        let paths: Vec<_> = (0..3)
            .map(|di| dir.join(format!("day-{di}.fdtrk")))
            .collect();
        let arrivals: Vec<JobResult> = vec![
            (0, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
            (1, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
            (
                0,
                synthetic_tag(Feed::Mlatonly),
                Ok(synthetic_part(&sp, "mlat")),
            ),
            (2, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
        ];
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (cached_tx, cached_rx) = std::sync::mpsc::sync_channel(0);
        let mut published = Vec::new();
        let mut retained: Vec<Option<CachedDay>> = (0..paths.len()).map(|_| None).collect();
        let failures = std::thread::scope(|s| {
            let paths = &paths;
            s.spawn(move || {
                let mut arrivals = arrivals.into_iter();
                for result in arrivals.by_ref().take(3) {
                    tx.send(result).unwrap();
                }
                // The final download waits for a real readable cache. Joining producers
                // before draining their bounded results would deadlock this handshake.
                cached_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                let first = TrackStore::load(&paths[0]).unwrap();
                assert_eq!(
                    first.meta.sources,
                    [synthetic_tag(Feed::Mlatonly), synthetic_tag(Feed::Prod)]
                );
                assert_eq!(first.legs.len(), 1);
                assert!(!first.legs[0].points.is_empty());
                assert!(paths[1].is_file());
                assert!(!paths[2].exists());
                tx.send(arrivals.next().unwrap()).unwrap();
            });
            collect_streamed_days(rx, vec![2, 1, 1], |di, parts, tags, fs| {
                assert_eq!(fs, if di == 0 { "prod+mlatonly" } else { "prod" });
                let store = build_store(parts, &sp, tags);
                retained[di] = Some(CachedDay::publish(store, &paths[di])?);
                published.push(di);
                if di == 0 {
                    cached_tx.send(()).unwrap();
                }
                Ok(())
            })
        });
        assert!(
            failures.feeds.is_empty() && failures.caches.is_empty(),
            "{failures:?}"
        );
        assert_eq!(published, [1, 0, 2]);
        let last = TrackStore::load(&paths[2]).unwrap();
        assert_eq!(last.legs.len(), 1);
        assert_eq!(last.meta.sources, [synthetic_tag(Feed::Prod)]);
        let selected = load_selected_stores(retained.into_iter().flatten().collect(), &sp).unwrap();
        assert_eq!(selected.len(), 3);
        assert!(selected.iter().all(|store| store.legs.len() == 1));
        assert_eq!(
            selected[0].meta.sources,
            [synthetic_tag(Feed::Mlatonly), synthetic_tag(Feed::Prod)]
        );
        assert_eq!(selected[1].meta.sources, [synthetic_tag(Feed::Prod)]);
        assert_eq!(selected[2].meta.sources, [synthetic_tag(Feed::Prod)]);
    }

    #[test]
    fn streamed_day_failures_preserve_main_feeds_and_other_days() {
        use crate::ingest::tests::spec;
        let sp = spec();
        let arrivals = vec![
            (
                0,
                synthetic_tag(Feed::Prod),
                Err(DataError("early failed main".into())),
            ),
            (1, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
            (
                0,
                synthetic_tag(Feed::Mlatonly),
                Ok(synthetic_part(&sp, "mlat")),
            ),
            (
                1,
                synthetic_tag(Feed::Mlatonly),
                Err(DataError("late failed MLAT".into())),
            ),
            (
                2,
                synthetic_tag(Feed::Mlatonly),
                Err(DataError("early failed MLAT".into())),
            ),
            (2, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
            (3, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
            (4, synthetic_tag(Feed::Prod), Ok(synthetic_part(&sp, "prod"))),
        ];
        let mut completed = Vec::new();
        let mut usable = Vec::new();
        let failures = collect_streamed_days(arrivals, vec![2, 2, 2, 1, 1], |di, parts, tags, fs| {
            completed.push(di);
            assert_eq!(fs, "prod", "MLAT-only remainder must not be published");
            assert_eq!(tags, [synthetic_tag(Feed::Prod)]);
            if di == 3 {
                return Err(DataError("cache write failed".into()));
            }
            let store = build_store(parts, &sp, tags);
            assert_eq!(store.legs.len(), 1);
            usable.push(di);
            Ok(())
        });
        assert_eq!(completed, [1, 2, 3, 4]);
        assert_eq!(usable, [1, 2, 4]);
        assert_eq!(
            failures.feeds,
            [
                (0, "early failed main".to_string()),
                (1, "late failed MLAT".to_string()),
                (2, "early failed MLAT".to_string()),
            ]
        );
        assert_eq!(failures.caches, [(3, "cache write failed".to_string())]);
        let permissive = FetchOptions {
            allow_missing: true,
            ..Default::default()
        };
        let mut notes = Vec::new();
        let error = failures.report(&permissive, &mut notes).unwrap_err();
        assert!(error.to_string().contains("cache write failed"));
        assert!(notes.is_empty(), "failed persistence must remain fatal");
    }

    #[test]
    fn failed_mlat_preserves_main_cache_for_permissive_and_offline_reuse() {
        use crate::ingest::tests::{T0, spec};
        let want = spec();
        let day = (2026, 9, 8);
        let (bbox, key) = bbox_key(&want.bbox);
        let cache_spec = IngestSpec {
            bbox,
            t_start: T0 - DAY_CACHE_PAD_S,
            t_end: T0 + 86_400.0 + DAY_CACHE_PAD_S,
            ..want
        };
        for main in [Feed::Prod, Feed::Staging] {
            for failed_first in [false, true] {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let cache = std::env::temp_dir().join(format!(
                    "fdop-partial-feed-{}-{nonce}-{}-{failed_first}",
                    std::process::id(),
                    main.tag_word()
                ));
                let part = synthetic_part(&cache_spec, main.label());
                let expected = build_day_store(
                    vec![part.clone()],
                    &cache_spec,
                    vec![synthetic_tag(main)],
                    day_context(day),
                );
                let mut arrivals = vec![
                    (0, synthetic_tag(main), Ok(part)),
                    (
                        0,
                        synthetic_tag(Feed::Mlatonly),
                        Err(DataError("synthetic MLAT outage".into())),
                    ),
                ];
                if failed_first {
                    arrivals.reverse();
                }
                let mut retained = None;
                let failures = collect_streamed_days(arrivals, vec![2], |di, parts, tags, fs| {
                    assert_eq!(di, 0);
                    assert_eq!(fs, main.tag_word());
                    let store = build_day_store(parts, &cache_spec, tags, day_context(day));
                    let path = day_dir(&cache, day).join(format!("{fs}_{key}.fdtrk"));
                    retained = Some(CachedDay::publish(store, &path)?);
                    Ok(())
                });
                assert_eq!(failures.feeds, [(0, "synthetic MLAT outage".to_string())]);
                assert!(failures.caches.is_empty());
                let saved = retained.as_ref().expect("successful main feed was discarded");
                assert_eq!(saved.meta.sources, [synthetic_tag(main)]);
                assert_eq!(saved.meta.bbox, cache_spec.bbox);
                assert_eq!(saved.meta.t_start, cache_spec.t_start);
                assert_eq!(saved.meta.t_end, cache_spec.t_end);
                assert_eq!(saved.meta.geoid_n_m, cache_spec.geoid_n_m);
                assert!(
                    DayContext::from_stats(&saved.meta.stats)
                        .is_some_and(|c| c.includes(day_context(day)))
                );
                let published = TrackStore::load(&saved.path).unwrap();
                assert_eq!(published.n_points(), expected.n_points());
                assert_eq!(published.legs[0].points, expected.legs[0].points);

                // Saving progress does not turn a failed strict online fetch into success.
                let strict = FetchOptions::default();
                let mut notes = Vec::new();
                let error = failures.report(&strict, &mut notes).unwrap_err();
                assert!(error.to_string().contains("synthetic MLAT outage"));
                assert!(notes.is_empty());
                // Explicitly permissive callers keep the successful day and the warning.
                let permissive = FetchOptions {
                    allow_missing: true,
                    ..strict.clone()
                };
                failures.report(&permissive, &mut notes).unwrap();
                assert_eq!(notes.len(), 1);
                assert!(notes[0].contains("synthetic MLAT outage"));
                let selected =
                    load_selected_stores(vec![retained.take().unwrap()], &want).unwrap();
                assert_eq!(selected.len(), 1);
                assert!(!selected[0].legs.is_empty());

                // The ordinary offline path already accepts a truthful lesser feedset,
                // reports its gap, and must now be able to reuse this durable main store.
                let offline = FetchOptions {
                    offline: true,
                    ..strict
                };
                let cached = fetch_window(&want, &cache, &offline).unwrap();
                assert_eq!(cached.meta.stats["cache_hits"], 1);
                assert_eq!(cached.meta.sources, [synthetic_tag(main)]);
                assert!(!cached.legs.is_empty());
                assert!(
                    cached.meta.stats["notes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|note| note.as_str().unwrap().contains(NO_MLATONLY))
                );
                assert!(
                    find_cached(
                        &day_dir(&cache, day),
                        &feedset_name(main, true),
                        &want,
                        "2026-09-08",
                        &offline,
                        Some(day_context(day))
                    )
                    .is_none(),
                    "partial cache must never satisfy the full-feedset lookup"
                );
            }
        }
    }

    #[test]
    fn offline_cache_roundtrip_and_geoid_adjust() {
        use crate::ingest::tests::{T0, spec};
        let sp = spec();
        // Production day caches ingest the whole day, including the later request below.
        let cache_spec = IngestSpec {
            t_start: T0,
            t_end: T0 + 86_400.0,
            ..sp
        };
        let (_, key) = bbox_key(&sp.bbox);
        let dir = std::env::temp_dir().join(format!("fdop-data-fetch-test-{}", std::process::id()));
        let store = synthetic_store(&cache_spec);
        let geom_alt = store.legs[0].points[2].alt_m;
        store
            .save(
                &dir.join("adsb")
                    .join("2026-09-08")
                    .join(format!("prod_{key}.fdtrk")),
            )
            .unwrap();
        let mut want = sp;
        want.geoid_n_m = -30.0;
        let opts = FetchOptions {
            include_mlat: false,
            offline: true,
            ..Default::default()
        };
        let got = fetch_window(&want, &dir, &opts).unwrap();
        assert_eq!(got.legs.len(), 1);
        assert_eq!(got.meta.stats["cache_hits"], 1);
        // GNSS heights move by N_old - N_new = -2 m
        assert!((got.legs[0].points[2].alt_m - (geom_alt - 2.0)).abs() < 1e-3);
        // An offline rerun with an agreed height cached at the region centre must
        // retain the original altitude reference instead of requesting the default.
        let geoid_dir = dir.join("geoid");
        std::fs::create_dir_all(&geoid_dir).unwrap();
        std::fs::write(
            geoid_dir.join("40.876466_-73.524951.txt"),
            sp.geoid_n_m.to_string(),
        )
        .unwrap();
        let (cached_n, source) =
            crate::geoid_height_with_options(&dir, 40.876466, -73.524951, true);
        assert_eq!(source, "ngs");
        let cached = fetch_window(
            &IngestSpec {
                geoid_n_m: cached_n,
                ..sp
            },
            &dir,
            &opts,
        )
        .unwrap();
        assert_eq!(cached.meta.geoid_n_m, store.meta.geoid_n_m);
        assert_eq!(cached.legs[0].points[2].alt_m, geom_alt);
        // a day that is not cached fails offline unless allow_missing
        let mut later = want;
        later.t_end += 86_400.0;
        assert!(fetch_window(&later, &dir, &opts).is_err());
        let ok = fetch_window(
            &later,
            &dir,
            &FetchOptions {
                allow_missing: true,
                ..opts.clone()
            },
        )
        .unwrap();
        assert_eq!(ok.legs.len(), 1);
    }

    #[test]
    fn offline_strict_type_policy_keeps_geometry_when_adjacent_day_supplies_type() {
        use fdop_types::store::TrackPoint;
        let midnight = fdop_types::time::unix_from_utc(2026, 9, 9, 0, 0, 0.0);
        let spec = IngestSpec {
            bbox: BBox::new(-0.02, -0.05, 0.02, 0.05),
            t_start: midnight - 1100.0,
            t_end: midnight + 1.0,
            geoid_n_m: 0.0,
            keep_unknown: false,
        };
        let point = |dt, lon| TrackPoint {
            t: midnight + dt,
            lat: 0.0,
            lon,
            alt_m: 1000.0,
            gs_mps: 70.0,
            track_deg: 90.0,
            vrate_mps: 0.0,
            alt_src: alt_src::GEOM,
            flags: 0,
        };
        let leg = |icao: &str, typ: &str, points| FlightLeg {
            key: format!("{icao}_original"),
            icao: icao.into(),
            reg: "TEST".into(),
            typ: typ.into(),
            source: "prod".into(),
            engine: fdop_types::engine::engine_class(typ),
            points,
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fdop-cross-day-type-{}-{nonce}",
            std::process::id()
        ));
        let (_, key) = bbox_key(&spec.bbox);
        let opts = FetchOptions {
            include_mlat: false,
            offline: true,
            ..Default::default()
        };
        // Either UTC day may have a missing or unrecognized aircraft type. The query
        // ends before the second day's reports, so losing the first half can lose the match.
        for (known_first, unknown_type) in
            [(false, ""), (true, ""), (false, "ZZZZ9"), (true, "ZZZZ9")]
        {
            for i in 0..2 {
                let points = if i == 0 {
                    vec![point(-20.0, -0.012), point(-10.0, -0.006)]
                } else {
                    vec![point(10.0, 0.006), point(20.0, 0.012)]
                };
                let mut legs = vec![
                    leg(
                        "000001",
                        if known_first == (i == 0) {
                            "PC12"
                        } else {
                            unknown_type
                        },
                        points.clone(),
                    ),
                    leg("000002", unknown_type, points),
                ];
                if i == 0 {
                    // This separate flight of the same aircraft must stay unknown;
                    // metadata is reconciled only across an actual day-store join.
                    legs.insert(
                        0,
                        leg(
                            "000001",
                            unknown_type,
                            vec![point(-1000.0, -0.003), point(-990.0, 0.003)],
                        ),
                    );
                }
                let day = midnight + (i as f64 - 1.0) * 86_400.0;
                TrackStore {
                    meta: StoreMeta {
                        version: 1,
                        bbox: spec.bbox,
                        t_start: day - 3600.0,
                        t_end: day + 86_400.0 + 3600.0,
                        geoid_n_m: 0.0,
                        sources: vec![format!("synthetic-day-{i}")],
                        stats: serde_json::json!({}),
                        created_unix: day,
                    },
                    legs,
                }
                .save(&day_dir(&dir, (2026, 9, 8 + i)).join(format!("prod_{key}.fdtrk")))
                .unwrap();
            }
            let strict = fetch_window(&spec, &dir, &opts).unwrap();
            assert_eq!(
                strict.legs.len(),
                1,
                "known_first={known_first}, unknown_type={unknown_type}"
            );
            let flight = &strict.legs[0];
            assert_eq!(flight.key, "000001_20260908_1");
            assert_eq!(flight.typ, "PC12");
            assert_eq!(flight.engine, EngineClass::Turboprop);
            assert_eq!(flight.points.len(), 4, "both halves must survive");
            assert_eq!(flight.t0(), midnight - 20.0);
            assert_eq!(flight.t1(), midnight + 20.0);
            assert_eq!(strict.meta.stats["cache_hits"], 2);
            let inclusive = fetch_window(
                &IngestSpec {
                    keep_unknown: true,
                    ..spec
                },
                &dir,
                &opts,
            )
            .unwrap();
            assert_eq!(inclusive.legs.len(), 3);
            let same_flight = inclusive.legs.iter().find(|l| l.typ == "PC12").unwrap();
            assert_eq!(flight.key, same_flight.key);
            assert_eq!(flight.points, same_flight.points);
        }
    }

    #[test]
    fn offline_cache_recovers_after_unreadable_candidates() {
        use crate::ingest::tests::spec;
        let sp = spec();
        for (include_mlat, good_feed) in
            [(false, "prod"), (true, "prod"), (true, "staging+mlatonly")]
        {
            let dir = std::env::temp_dir().join(format!(
                "fdop-data-cache-recovery-{}-{include_mlat}-{good_feed}",
                std::process::id()
            ));
            let day = day_dir(&dir, (2026, 9, 8));
            std::fs::create_dir_all(&day).unwrap();
            let (_, small_key) = bbox_key(&sp.bbox);
            let bad_path = day.join(format!("prod_{small_key}.fdtrk"));
            std::fs::write(&bad_path, b"not a gzip track store").unwrap();
            let larger = IngestSpec {
                bbox: BBox::new(39.0, -75.0, 42.0, -72.0),
                ..sp
            };
            let (_, large_key) = bbox_key(&larger.bbox);
            let good_path = day.join(format!("{good_feed}_{large_key}.fdtrk"));
            synthetic_store(&larger).save(&good_path).unwrap();
            let messages = Arc::new(Mutex::new(Vec::<String>::new()));
            let captured = messages.clone();
            let opts = FetchOptions {
                include_mlat,
                offline: true,
                progress: Some(Arc::new(move |s| {
                    captured.lock().unwrap().push(s.to_string());
                })),
                ..Default::default()
            };
            let got = fetch_window(&sp, &dir, &opts).unwrap();
            assert_eq!(got.legs.len(), 1);
            assert_eq!(got.legs[0].icao, "a1b2c3");
            assert_eq!(got.meta.stats["cache_hits"], 1);
            let notes = got.meta.stats["notes"].as_array().unwrap();
            assert_eq!(notes.is_empty(), !include_mlat);
            if include_mlat {
                let expected = if good_feed == "prod" {
                    NO_MLATONLY
                } else {
                    "prod release missing, using staging"
                };
                assert!(notes.iter().any(|n| n.as_str().unwrap().contains(expected)));
            }
            let log = messages.lock().unwrap().join("\n");
            assert!(log.contains(&format!("unreadable cache {}", bad_path.display())));
            assert!(log.contains(&format!("cached {}", good_path.display())));
            assert!(!log.contains("refetching"));
        }
    }

    #[test]
    fn offline_cache_requires_readable_covering_metadata() {
        use crate::ingest::tests::spec;
        let sp = spec();
        for failure in ["unreadable", "bbox", "time_start", "time_end"] {
            let dir = std::env::temp_dir().join(format!(
                "fdop-data-cache-coverage-{}-{failure}",
                std::process::id()
            ));
            let day = day_dir(&dir, (2026, 9, 8));
            std::fs::create_dir_all(&day).unwrap();
            let (_, key) = bbox_key(&sp.bbox);
            let path = day.join(format!("prod_{key}.fdtrk"));
            if failure == "unreadable" {
                std::fs::write(&path, b"not a gzip track store").unwrap();
            } else {
                let mut incomplete = sp;
                match failure {
                    "bbox" => incomplete.bbox.west += 0.1,
                    "time_start" => incomplete.t_start += 1.0,
                    "time_end" => incomplete.t_end -= 1.0,
                    _ => unreachable!(),
                }
                // Its filename claims full coverage; the actual ingest metadata disagrees.
                synthetic_store(&incomplete).save(&path).unwrap();
            }
            let opts = FetchOptions {
                include_mlat: false,
                offline: true,
                ..Default::default()
            };
            let error = fetch_window(&sp, &dir, &opts).unwrap_err().to_string();
            assert!(
                error.contains("2026-09-08 (offline, not cached)"),
                "{error}"
            );
        }
    }

    fn cached_subset_fixture() -> (IngestSpec, Vec<TrackStore>) {
        use fdop_types::store::{TrackPoint, pt_flags};
        let (lat, lon) = (40.876466, -73.524951);
        let midnight = fdop_types::time::unix_from_utc(2026, 9, 9, 0, 0, 0.0);
        let spec = IngestSpec {
            bbox: BBox::new(lat - 0.002, lon - 0.002, lat + 0.002, lon + 0.002),
            t_start: midnight - 10.0,
            t_end: midnight + 10.0,
            geoid_n_m: -28.0,
            keep_unknown: true,
        };
        let point = |t: f64, dx: f64, dy: f64| TrackPoint {
            t: midnight + t,
            lat: lat + dy,
            lon: lon + dx,
            alt_m: 1200.0,
            gs_mps: 70.0,
            track_deg: 90.0,
            vrate_mps: 0.0,
            alt_src: alt_src::GEOM,
            flags: 0,
        };
        let leg = |icao: &str, tag: &str, engine, points| FlightLeg {
            key: format!("{icao}_{tag}"),
            icao: icao.into(),
            reg: "TEST".into(),
            typ: if engine == EngineClass::Unknown {
                ""
            } else {
                "C172"
            }
            .into(),
            source: "prod".into(),
            engine,
            points,
        };
        let early = leg(
            "000001",
            "early",
            EngineClass::Piston,
            vec![point(-7200.0, 1.0, 0.1), point(-7100.0, 1.1, 0.1)],
        );
        let left = leg(
            "000001",
            "left",
            EngineClass::Piston,
            vec![
                point(-120.0, -0.09, 0.0),
                point(-80.0, -0.06, 0.0),
                point(-40.0, -0.03, 0.0),
            ],
        );
        let mut right = leg(
            "000001",
            "right",
            EngineClass::Piston,
            vec![
                point(40.0, 0.03, 0.0),
                point(80.0, 0.06, 0.0),
                point(120.0, 0.09, 0.0),
            ],
        );
        right.source = "mlat".into();
        let far = leg(
            "000002",
            "far",
            EngineClass::Piston,
            vec![point(-120.0, 1.0, 1.0), point(120.0, 1.1, 1.0)],
        );
        let past = leg(
            "000003",
            "past",
            EngineClass::Piston,
            vec![point(-4000.0, -0.01, 0.0), point(-3900.0, 0.01, 0.0)],
        );
        let unknown = leg(
            "000004",
            "unknown",
            EngineClass::Unknown,
            vec![point(-20.0, -0.015, 0.0), point(20.0, 0.015, 0.0)],
        );
        let mut noisy = leg(
            "000005",
            "noisy",
            EngineClass::Piston,
            vec![
                point(-100.0, -0.075, 0.001),
                point(0.0, 0.0, 0.001),
                point(100.0, 0.075, 0.001),
            ],
        );
        for p in &mut noisy.points {
            p.flags = pt_flags::MLAT;
        }
        noisy.points[0].alt_src = alt_src::BARO_RAW;
        let precise = leg(
            "000005",
            "precise",
            EngineClass::Piston,
            vec![point(0.03125, 0.0, 0.0), point(100.03125, 0.075, 0.0)],
        );
        let stores = [
            vec![early, left, far.clone(), past, unknown, noisy],
            vec![right, precise, far],
        ]
        .into_iter()
        .enumerate()
        .map(|(i, legs)| {
            let day = midnight + (i as f64 - 1.0) * 86_400.0;
            TrackStore {
                meta: StoreMeta {
                    version: 1,
                    bbox: BBox::new(lat - 2.0, lon - 2.0, lat + 2.0, lon + 2.0),
                    t_start: day - 3600.0,
                    t_end: day + 86_400.0 + 3600.0,
                    geoid_n_m: if i == 0 { -32.0 } else { -20.0 },
                    sources: vec![format!("synthetic-day-{i}")],
                    stats: serde_json::json!({"fixture_day": i, "legs": legs.len()}),
                    created_unix: day,
                },
                legs,
            }
        })
        .collect();
        (spec, stores)
    }

    #[test]
    fn cached_subset_matches_the_full_cross_day_merge_and_metadata() {
        let (mut spec, original) = cached_subset_fixture();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("fdop-cached-subset-{}-{nonce}", std::process::id()));
        let mut paths = Vec::new();
        for (i, store) in original.iter().enumerate() {
            let day = (2026, 9, 8 + i as u32);
            let (_, key) = bbox_key(&store.meta.bbox);
            // The second day deliberately uses a partial feed cache.
            let feed = if i == 0 { "prod+mlatonly" } else { "prod" };
            let path = day_dir(&dir, day).join(format!("{feed}_{key}.fdtrk"));
            store.save(&path).unwrap();
            paths.push(path);
            assert!(
                store
                    .filtered(&spec.bbox, spec.t_start, spec.t_end)
                    .legs
                    .iter()
                    .all(|l| l.icao != "000001"),
                "neither separate day contains the requested crossing"
            );
        }
        // An earlier, smaller candidate has intact records but a truncated footer.
        // A scanner must validate discarded data before admitting any of its summaries.
        let (_, small_key) = bbox_key(&spec.bbox);
        let bad = day_dir(&dir, (2026, 9, 8)).join(format!("prod+mlatonly_{small_key}.fdtrk"));
        original[0].save(&bad).unwrap();
        let bytes = std::fs::read(&bad).unwrap();
        std::fs::write(&bad, &bytes[..bytes.len() - 4]).unwrap();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let captured = messages.clone();
        let opts = FetchOptions {
            offline: true,
            progress: Some(Arc::new(move |s| {
                captured.lock().unwrap().push(s.to_string());
            })),
            ..Default::default()
        };
        for keep_unknown in [true, false] {
            spec.keep_unknown = keep_unknown;
            let mut full = original.clone();
            for store in &mut full {
                adjust_geoid(store, spec.geoid_n_m);
            }
            let mut merged = merge_stores(full, DAY_JOIN_GAP_S);
            if !keep_unknown {
                merged.legs.retain(|l| l.engine != EngineClass::Unknown);
            }
            let expected = merged.filtered(&spec.bbox, spec.t_start, spec.t_end);
            let got = fetch_window(&spec, &dir, &opts).unwrap();
            assert_eq!(got.legs.len(), expected.legs.len());
            for (actual, expected) in got.legs.iter().zip(&expected.legs) {
                assert_eq!(actual.key, expected.key);
                assert_eq!(actual.icao, expected.icao);
                assert_eq!(actual.reg, expected.reg);
                assert_eq!(actual.typ, expected.typ);
                assert_eq!(actual.engine, expected.engine);
                assert_eq!(actual.source, expected.source);
                assert_eq!(actual.points, expected.points);
            }
            let crossing = got.legs.iter().find(|l| l.icao == "000001").unwrap();
            assert_eq!(crossing.key, "000001_20260908_1");
            assert_eq!(crossing.points.len(), 6);
            assert_eq!(got.legs.iter().any(|l| l.icao == "000004"), keep_unknown);
            assert_eq!(got.meta.sources, expected.meta.sources);
            assert_eq!(got.meta.bbox, spec.bbox);
            assert_eq!(
                (got.meta.t_start, got.meta.t_end),
                (spec.t_start, spec.t_end)
            );
            assert_eq!(got.meta.geoid_n_m, spec.geoid_n_m);
            assert_eq!(got.meta.stats["cache_hits"], 2);
            let day_stats: Vec<_> = original
                .iter()
                .map(|s| serde_json::json!({"sources": s.meta.sources, "stats": s.meta.stats}))
                .collect();
            assert_eq!(got.meta.stats["day_stores"], serde_json::json!(day_stats));
            let notes = got.meta.stats["notes"].as_array().unwrap();
            assert_eq!(notes.len(), 3);
            assert_eq!(
                notes
                    .iter()
                    .filter(|n| n.as_str().unwrap().contains(NO_MLATONLY))
                    .count(),
                1
            );
            for day in ["2026-09-08", "2026-09-09"] {
                let prefix = format!("{day}: legacy day cache lacks verified midnight coverage");
                let warnings: Vec<_> = notes
                    .iter()
                    .filter_map(|n| n.as_str())
                    .filter(|n| n.starts_with(&prefix))
                    .collect();
                assert_eq!(
                    warnings.len(),
                    1,
                    "missing or repeated coverage note for {day}"
                );
                assert!(
                    warnings[0].contains("cross-midnight flights may be missing or discontinuous")
                );
                assert!(warnings[0].contains("rerun online"));
            }
        }
        assert!(
            messages.lock().unwrap().iter().any(|s| {
                s.contains("unreadable cache") && s.contains(&bad.display().to_string())
            })
        );

        // The same selection works with one freshly published day. It discards unrelated
        // aircraft but retains the selected aircraft's earlier leg and both original metas.
        let selected = load_selected_stores(
            vec![
                CachedDay::scan(&paths[0]).unwrap(),
                CachedDay::publish(original[1].clone(), &paths[1]).unwrap(),
            ],
            &spec,
        )
        .unwrap();
        assert!(
            selected.iter().map(TrackStore::n_points).sum::<usize>()
                < original.iter().map(TrackStore::n_points).sum::<usize>()
        );
        assert_eq!(
            selected[0]
                .legs
                .iter()
                .filter(|l| l.icao == "000001")
                .count(),
            2
        );
        for (kept, all) in selected.iter().zip(&original) {
            assert!(
                kept.legs
                    .iter()
                    .all(|l| matches!(l.icao.as_str(), "000001" | "000004" | "000005"))
            );
            assert_eq!(
                serde_json::to_value(&kept.meta).unwrap(),
                serde_json::to_value(&all.meta).unwrap()
            );
        }
        let (lat, lon) = spec.bbox.center();
        let empty_spec = IngestSpec {
            bbox: BBox::new(lat + 0.4, lon, lat + 0.5, lon + 0.1),
            ..spec
        };
        let empty = load_selected_stores(
            paths.iter().map(|p| CachedDay::scan(p).unwrap()).collect(),
            &empty_spec,
        )
        .unwrap();
        assert_eq!(empty.len(), original.len());
        for (kept, all) in empty.iter().zip(&original) {
            assert!(kept.legs.is_empty());
            assert_eq!(
                serde_json::to_value(&kept.meta).unwrap(),
                serde_json::to_value(&all.meta).unwrap()
            );
        }
    }

    #[test]
    fn published_days_match_the_full_merge_after_cache_replacement() {
        let (mut spec, mut original) = cached_subset_fixture();
        let mut later = original[1].clone();
        later.legs.retain(|leg| leg.icao == "000002");
        for leg in &mut later.legs {
            for p in &mut leg.points {
                p.t += 86_400.0;
            }
        }
        later.meta.t_start += 86_400.0;
        later.meta.t_end += 86_400.0;
        later.meta.created_unix += 86_400.0;
        later.meta.sources = vec!["synthetic-day-2".into()];
        later.meta.stats = serde_json::json!({"fixture_day": 2, "legs": later.legs.len()});
        original.push(later);
        spec.t_end += 86_400.0;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fdop-published-days-{}-{nonce}",
            std::process::id()
        ));
        for keep_unknown in [true, false] {
            spec.keep_unknown = keep_unknown;
            let published: Vec<_> = original
                .iter()
                .enumerate()
                .map(|(i, store)| {
                    let path = dir.join(format!("day-{i}.fdtrk"));
                    let day = CachedDay::publish(store.clone(), &path).unwrap();
                    assert_eq!(day.leg_count, store.legs.len());
                    // Another publisher can replace every public cache name while
                    // later downloads continue. Our summaries still describe our files.
                    let mut replacement = store.clone();
                    replacement.legs.clear();
                    replacement.meta.sources = vec!["replacement".into()];
                    replacement.save(&path).unwrap();
                    assert!(TrackStore::load(&path).unwrap().legs.is_empty());
                    day
                })
                .collect();
            let selected = load_selected_stores(published, &spec).unwrap();
            assert_eq!(selected.len(), 3);
            assert!(selected[2].legs.is_empty());
            assert!(
                selected.iter().map(TrackStore::n_points).sum::<usize>()
                    < original.iter().map(TrackStore::n_points).sum::<usize>()
            );
            for (kept, all) in selected.iter().zip(&original) {
                assert_eq!(
                    serde_json::to_value(&kept.meta).unwrap(),
                    serde_json::to_value(&all.meta).unwrap()
                );
            }
            let finish = |mut stores: Vec<TrackStore>| {
                for store in &mut stores {
                    adjust_geoid(store, spec.geoid_n_m);
                }
                let mut merged = merge_stores(stores, DAY_JOIN_GAP_S);
                if !keep_unknown {
                    merged.legs.retain(|leg| leg.engine != EngineClass::Unknown);
                }
                merged.filtered(&spec.bbox, spec.t_start, spec.t_end)
            };
            let expected = finish(original.clone());
            let got = finish(selected);
            assert_eq!(got.legs.len(), expected.legs.len());
            for (actual, expected) in got.legs.iter().zip(&expected.legs) {
                assert_eq!(actual.key, expected.key);
                assert_eq!(actual.icao, expected.icao);
                assert_eq!(actual.reg, expected.reg);
                assert_eq!(actual.typ, expected.typ);
                assert_eq!(actual.engine, expected.engine);
                assert_eq!(actual.source, expected.source);
                assert_eq!(actual.points, expected.points);
            }
            let crossing = got.legs.iter().find(|leg| leg.icao == "000001").unwrap();
            assert_eq!(crossing.key, "000001_20260908_1");
            assert_eq!(crossing.points.len(), 6);
            assert_eq!(
                got.legs.iter().any(|leg| leg.icao == "000004"),
                keep_unknown
            );
            let adjusted = got.legs.iter().find(|leg| leg.icao == "000005").unwrap();
            assert_eq!(adjusted.points[0].alt_src, alt_src::BARO_RAW);
            assert_eq!(adjusted.points[0].alt_m, 1200.0);
            assert_eq!(adjusted.points[1].alt_m, 1208.0);
            assert_eq!(got.meta.sources, expected.meta.sources);
            assert_eq!(got.meta.bbox, expected.meta.bbox);
            assert_eq!(got.meta.t_start, expected.meta.t_start);
            assert_eq!(got.meta.t_end, expected.meta.t_end);
            assert_eq!(got.meta.geoid_n_m, expected.meta.geoid_n_m);
        }
    }

    #[test]
    fn aircraft_bounds_keep_wraparound_and_uncertain_reports() {
        let (spec, stores) = cached_subset_fixture();
        let mut b = AircraftBounds {
            t0: spec.t_start,
            t1: spec.t_end,
            bbox: BBox::new(-1.0, -179.0, 1.0, 179.0),
            uncertain: false,
        };
        let dateline = IngestSpec {
            bbox: BBox::new(-0.5, 179.5, 0.5, 180.0),
            ..spec
        };
        assert!(
            b.overlaps(&dateline),
            "a wrapped segment can enter this box"
        );
        b.bbox = BBox::new(-1.0, -10.0, 1.0, 10.0);
        assert!(!b.overlaps(&dateline));
        assert!(!AircraftBounds::default().overlaps(&spec));
        let mut leg = stores[0].legs[0].clone();
        leg.points[0].t = f64::NAN;
        let mut bounds = HashMap::new();
        include_leg_bounds(&mut bounds, &leg);
        assert!(bounds[&leg.icao].overlaps(&dateline));
    }

    #[test]
    fn aircraft_bounds_preserve_exact_dateline_and_rounded_endpoint_hits() {
        let (base, stores) = cached_subset_fixture();
        let (lat, lon) = base.bbox.center();
        let check = |name: &str, a: (f64, f64, f64), b: (f64, f64, f64), spec: IngestSpec| {
            let mut leg = stores[0].legs[0].clone();
            for (p, (t, lat, lon)) in leg.points.iter_mut().zip([a, b]) {
                p.t = t;
                p.lat = lat;
                p.lon = lon;
            }
            let full = TrackStore {
                meta: stores[0].meta.clone(),
                legs: vec![leg],
            };
            let expected = full.filtered(&spec.bbox, spec.t_start, spec.t_end);
            assert_eq!(expected.legs.len(), 1, "existing filter must keep {name}");
            assert_eq!(expected.legs[0].points.len(), 2);
            let mut bounds = HashMap::new();
            include_leg_bounds(&mut bounds, &full.legs[0]);
            assert!(
                bounds[&full.legs[0].icao].overlaps(&spec),
                "the prefilter must retain the existing filter's {name}"
            );
        };
        // The existing clip treats -180 and +180 as the same meridian, including
        // constant-longitude segments and a segment touching only at its endpoint.
        for sign in [-1.0, 1.0] {
            let west = if sign < 0.0 { 179.5 } else { -180.0 };
            let spec = IngestSpec {
                bbox: BBox::new(lat - 0.01, west, lat + 0.01, west + 0.5),
                t_start: 0.0,
                t_end: 10.0,
                ..base
            };
            for end_lon in [sign * 180.0, sign * 170.0] {
                check(
                    "equivalent dateline meridian",
                    (0.0, lat, sign * 180.0),
                    (10.0, lat, end_lon),
                    spec,
                );
            }
        }
        // These probes deliberately sit one representable step outside the exact
        // endpoint. The existing clip's subtraction/division rounds them onto it.
        let just_after_one = 1.0 + f64::EPSILON;
        check(
            "rounded longitude",
            (0.0, lat, -179.0),
            (10.0, lat, 1.0),
            IngestSpec {
                bbox: BBox::new(lat - 0.01, just_after_one, lat + 0.01, 1.1),
                t_start: 0.0,
                t_end: 10.0,
                ..base
            },
        );
        check(
            "rounded latitude",
            (0.0, -89.0, lon),
            (10.0, 1.0, lon),
            IngestSpec {
                bbox: BBox::new(just_after_one, lon - 0.01, 1.1, lon + 0.01),
                t_start: 0.0,
                t_end: 10.0,
                ..base
            },
        );
        check(
            "rounded time",
            (-179.0, lat, lon),
            (1.0, lat, lon),
            IngestSpec {
                t_start: just_after_one,
                t_end: 2.0,
                ..base
            },
        );
    }

    #[test]
    fn recent_unpublished_days_are_pending() {
        let dir =
            std::env::temp_dir().join(format!("fdop-data-pending-test-{}", std::process::id()));
        let d8 = dir.join("adsb").join("2026-09-08");
        std::fs::create_dir_all(&d8).unwrap();
        std::fs::write(d8.join("prod_40.0_-74.0_41.0_-73.0.fdtrk"), b"x").unwrap();
        let t8 = fdop_types::time::parse_iso_utc("2026-09-08T00:00:00Z").unwrap();
        let day = 86_400.0;
        // 09-09 is over but young and has no archive; 09-08 is cached
        let (until, pend) = published_until(
            t8 + 3600.0,
            t8 + day + 7200.0,
            &dir,
            true,
            t8 + 2.0 * day + 3600.0,
        )
        .unwrap();
        assert_eq!(until, t8 + day);
        assert_eq!(pend, vec!["2026-09-09".to_string()]);
        // days that have not ended cannot be published, cache or not
        let (until, pend) = published_until(t8, t8 + day, &dir, true, t8 + 5.0 * 3600.0).unwrap();
        assert_eq!(until, t8);
        assert_eq!(pend.len(), 2);
        // an old missing day is not pending: fetch_window reports it as missing
        let t1 = t8 + day + 7200.0;
        assert_eq!(
            published_until(t8, t1, &dir, true, t8 + 10.0 * day).unwrap(),
            (t1, vec![])
        );
    }
}
