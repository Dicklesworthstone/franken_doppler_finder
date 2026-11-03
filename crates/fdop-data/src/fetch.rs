//! Fetch the tracks for a search window: per UTC day, use a cached day store whose bbox
//! covers the request, otherwise stream the day's adsb.lol release(s) and cache the result.
//!
//! Cache layout: `<cache>/adsb/<YYYY-MM-DD>/<feedset>_<S>_<W>_<N>_<E>.fdtrk` (bbox rounded
//! outward to 0.1 degree; the whole UTC day is kept, the window is applied at load) and
//! `<cache>/adsb/<YYYY-MM-DD>/releases.json`. Day stores are always built with unknown types
//! kept; `keep_unknown = false` is applied at load.

use crate::DataError;
use crate::adsblol::{DayRelease, Feed, find_day_releases_cached};
use crate::build::{build_store, merge_stores};
use crate::ingest::{IngestProgress, IngestSpec, IngestStats, RawAircraft, ingest_reader};
use fdop_types::BBox;
use fdop_types::engine::EngineClass;
use fdop_types::store::{TrackStore, alt_src};
use fdop_types::time::{civil_from_days, days_from_civil};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Legs of one aircraft in consecutive day stores closer than this are joined (s).
pub const DAY_JOIN_GAP_S: f64 = 300.0;

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

/// Smallest cached day store for `feedset` whose bbox covers `want`.
fn find_cached(dir: &Path, feedset: &str, want: &BBox) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(f64, PathBuf)> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some((fs, bb)) = parse_cache_name(&name)
            && fs == feedset
            && covers(&bb, want)
        {
            let a = bb.area_km2();
            if best.as_ref().is_none_or(|(ba, _)| a < *ba) {
                best = Some((a, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Cache feedset name of a day store built from `main` (prod or staging) plus optional mlatonly.
fn feedset_name(main: Feed, mlat: bool) -> String {
    format!("{}{}", main.tag_word(), if mlat { "+mlatonly" } else { "" })
}

/// Feedset of a store built from release `tags` (e.g. `v2026.11.02-planes-readsb-prod-0`).
fn feedset_of_tags(tags: &[String]) -> String {
    let has = |f: Feed| {
        tags.iter()
            .any(|t| t.contains(&format!("-{}-", f.tag_word())))
    };
    let main = if has(Feed::Prod) {
        Feed::Prod
    } else {
        Feed::Staging
    };
    feedset_name(main, has(Feed::Mlatonly))
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
    let mut day_stores: Vec<Option<TrackStore>> = vec![None; days.len()];
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
        if let Some(p) = find_cached(&dir, feedset, &spec.bbox) {
            match TrackStore::load(&p) {
                Ok(s) => {
                    opts.say(&format!(
                        "[adsb] {label}: cached {} ({} legs)",
                        p.display(),
                        s.legs.len()
                    ));
                    day_stores[i] = Some(s);
                    cache_hits += 1;
                    continue;
                }
                Err(e) => opts.say(&format!(
                    "[adsb] {label}: unreadable cache {} ({e}); refetching",
                    p.display()
                )),
            }
        }
        // a store built while a release was missing: use it when nothing better exists
        let partial = partial_feedsets(feedset)
            .iter()
            .find_map(|fs| find_cached(&dir, fs, &spec.bbox).map(|p| (p, *fs)));
        let rels = if opts.offline {
            Vec::new()
        } else {
            find_day_releases_cached(net.as_ref(), day.0, day.1, day.2, &dir, false)?
        };
        if let Some((p, fs)) = &partial
            && (opts.offline
                || feedset_available(&rels, opts.include_mlat).is_none_or(|a| a.as_str() == *fs))
        {
            match TrackStore::load(p) {
                Ok(s) => {
                    opts.say(&format!(
                        "[adsb] {label}: cached {} ({} legs; {fs} only, nothing better is published)",
                        p.display(),
                        s.legs.len()
                    ));
                    notes.push(format!("{label}: {fs} only"));
                    day_stores[i] = Some(s);
                    cache_hits += 1;
                    continue;
                }
                Err(e) => opts.say(&format!(
                    "[adsb] {label}: unreadable cache {} ({e}); refetching",
                    p.display()
                )),
            }
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
                None => notes.push(format!("{label}: no mlatonly release")),
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
        t_start: days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0 - 3600.0,
        t_end: days_from_civil(day.0, day.1, day.2) as f64 * 86_400.0 + 86_400.0 + 3600.0,
        geoid_n_m: spec.geoid_n_m,
        keep_unknown: true,
    };
    let mut per_day: BTreeMap<usize, (Vec<Part>, Vec<String>)> = BTreeMap::new();
    let mut failed: Vec<(usize, String)> = Vec::new();
    if !jobs.is_empty() {
        let total: u64 = jobs.iter().map(|j| j.rel.total_bytes()).sum();
        opts.say(&format!(
            "[adsb] streaming {} release(s), {:.0} MiB, {} at a time",
            jobs.len(),
            mib(total),
            opts.parallel.max(1)
        ));
        let queue = Mutex::new(jobs.into_iter().collect::<VecDeque<Job>>());
        let results: Mutex<Vec<JobResult>> = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..opts.parallel.max(1) {
                s.spawn(|| {
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
                        let res = ingest_reader(reader, job.rel.feed.label(), &sp, &cb)
                            .map_err(|e| DataError(format!("{label} ({}): {e}", job.rel.tag)));
                        if let Ok(mut r) = results.lock() {
                            r.push((job.day_idx, job.rel.tag.clone(), res));
                        }
                    }
                });
            }
        });
        let results = results
            .into_inner()
            .map_err(|_| DataError("result lock poisoned".into()))?;
        // keep every day whose releases all streamed; a failed stream must not throw away the
        // other days' finished (and expensive) downloads
        for (di, tag, res) in results {
            match res {
                Ok(part) => {
                    let e = per_day.entry(di).or_default();
                    e.0.push(part);
                    e.1.push(tag);
                }
                Err(e) => failed.push((di, e.0)),
            }
        }
        for (di, _) in &failed {
            per_day.remove(di);
        }
    }
    for (di, (parts, mut tags)) in per_day {
        tags.sort();
        let day = days[di];
        // named by what was actually streamed, so a day fetched before its mlatonly (or prod)
        // release existed is not mistaken for a complete one later
        let fs = feedset_of_tags(&tags);
        let store = build_store(parts, &day_spec(day), tags);
        let dir = day_dir(cache_dir, day);
        let path = dir.join(format!("{fs}_{key}.fdtrk"));
        store.save(&path)?;
        opts.say(&format!(
            "[adsb] {:04}-{:02}-{:02}: built {} legs / {} points -> {}",
            day.0,
            day.1,
            day.2,
            store.legs.len(),
            store.n_points(),
            path.display()
        ));
        day_stores[di] = Some(store);
    }

    if !failed.is_empty() {
        let msg = format!(
            "ADS-B download failed (finished days were cached; rerun to retry the rest): {}",
            failed
                .iter()
                .map(|(_, e)| e.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        if !opts.allow_missing {
            return Err(DataError(msg));
        }
        opts.say(&format!("[adsb] WARNING {msg}"));
        notes.push(msg);
    }

    // ---- combine ----
    let mut stores: Vec<TrackStore> = day_stores.into_iter().flatten().collect();
    if stores.is_empty() {
        return Err(DataError(
            "no ADS-B day stores available for the window".into(),
        ));
    }
    let mut day_stats = Vec::new();
    for s in &mut stores {
        adjust_geoid(s, spec.geoid_n_m);
        if !spec.keep_unknown {
            s.legs.retain(|l| l.engine != EngineClass::Unknown);
        }
        day_stats.push(serde_json::json!({"sources": s.meta.sources, "stats": s.meta.stats}));
    }
    let merged = merge_stores(stores, DAY_JOIN_GAP_S);
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
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn offline_cache_roundtrip_and_geoid_adjust() {
        use crate::ingest::tests::{gz, pc12_json, spec, tar_of};
        let sp = spec();
        let tarb = tar_of(&[("./traces/c3/trace_full_a1b2c3.json", gz(&pc12_json()))]);
        let part = ingest_reader(std::io::Cursor::new(tarb), "prod", &sp, &|_| {}).unwrap();
        let (_, key) = bbox_key(&sp.bbox);
        let dir = std::env::temp_dir().join(format!("fdop-data-fetch-test-{}", std::process::id()));
        let store = build_store(vec![part], &sp, vec!["synthetic".into()]);
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
