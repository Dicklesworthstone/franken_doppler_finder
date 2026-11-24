//! Private checkpoints of completely ingested release parts. Retain raw altitude pairs
//! and boundary provenance so a later feed is reconciled by the ordinary day builder.
//! This is disposable cache data, not a public interchange format: a version, release,
//! coverage or policy mismatch is a miss and an unreadable file is refetched. A change
//! only to the geoid re-references GNSS heights without discarding the completed ingest.
//! After an exact miss, a covering spatial checkpoint can supply a narrower retry. An
//! ordinary request may also remove known jets from a certified jet-inclusive raw feed;
//! every ordinary checkpoint keeps priority, and all other identity fields still match.

use crate::DataError;
use crate::adsblol::{DayRelease, Feed};
use crate::ingest::{DayContext, IngestSpec, IngestStats, RawAircraft};
use fdop_types::BBox;
use fdop_types::store::TrackStore;
use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bump when ingest semantics or the private encoding change.
const VERSION: u8 = 1;

#[derive(Serialize, Deserialize, PartialEq)]
struct Identity {
    version: u8,
    release: DayRelease,
    filter_bits: [u64; 7],
    keep_unknown: bool,
    include_jets: bool,
    context_bits: [u64; 4],
}

impl Identity {
    fn new(rel: &DayRelease, spec: &IngestSpec, context: DayContext) -> Self {
        Self {
            version: VERSION,
            release: rel.clone(),
            filter_bits: [
                spec.bbox.south.to_bits(),
                spec.bbox.west.to_bits(),
                spec.bbox.north.to_bits(),
                spec.bbox.east.to_bits(),
                spec.t_start.to_bits(),
                spec.t_end.to_bits(),
                spec.geoid_n_m.to_bits(),
            ],
            keep_unknown: spec.keep_unknown,
            include_jets: spec.include_jets,
            context_bits: context.cache_identity(),
        }
    }
}

#[derive(Serialize)]
struct CheckpointRef<'a> {
    identity: &'a Identity,
    aircraft: &'a [RawAircraft],
    stats: &'a IngestStats,
}

#[derive(Deserialize)]
struct Checkpoint {
    identity: Identity,
    aircraft: Vec<RawAircraft>,
    stats: IngestStats,
}

/// Recover missing release identities locally, never as a source for network downloads.
/// A valid sidecar keeps authority over its listed feed and its main-feed selection.
/// Reading only the identity avoids allocating the aircraft twice, while deserializing
/// the whole document still checks JSON and the completed gzip before using metadata.
pub(crate) fn recovery_releases(
    dir: &Path,
    listed: &[DayRelease],
    context: DayContext,
    include_mlat: bool,
) -> Vec<DayRelease> {
    #[derive(Deserialize)]
    struct Header {
        identity: Identity,
        #[serde(rename = "aircraft")]
        _aircraft: Vec<serde::de::IgnoredAny>,
        stats: IngestStats,
    }

    let day = fdop_types::time::civil_from_days((context.day_start / 86_400.0).floor() as i64);
    let valid = |r: &DayRelease| {
        crate::fetch::release_for_day(r, day)
            && crate::adsblol::valid_archive_parts(&r.tag, &r.parts)
    };
    let main_listed = listed.iter().any(|r| r.feed != Feed::Mlatonly && valid(r));
    if main_listed && (!include_mlat || listed.iter().any(|r| r.feed == Feed::Mlatonly && valid(r))) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir.join("raw")) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((label, bbox)) = name
            .strip_suffix(".fdopraw.gz")
            .and_then(|stem| crate::fetch::parse_cache_name(&format!("{stem}.fdtrk")))
        else {
            continue;
        };
        let (label, jets) = label
            .strip_suffix("+jets")
            .map_or((label.as_str(), false), |s| (s, true));
        let Some(feed) = Feed::from_label(label) else {
            continue;
        };
        if feed.tag_word() != label
            || !covers_bbox(bbox, bbox)
            || (feed == Feed::Mlatonly && !include_mlat)
            || (feed != Feed::Mlatonly && main_listed)
            || listed.iter().any(|r| r.feed == feed && valid(r))
        {
            continue;
        }
        candidates.push((jets, bbox.area_km2(), entry.path(), feed));
    }
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut recovered = Vec::new();
    for (jets, _, path, feed) in candidates {
        let Ok(file) = File::open(path) else { continue };
        let Ok(header) =
            serde_json::from_reader::<_, Header>(BufReader::new(MultiGzDecoder::new(file)))
        else {
            continue;
        };
        let id = header.identity;
        if crate::ingest::validate_trace_counts(header.stats.members, header.stats.parse_errors).is_ok()
            && id.version == VERSION
            && id.release.feed == feed
            && id.include_jets == jets
            && id.keep_unknown
            && id.context_bits == context.cache_identity()
            && valid(&id.release)
            && !recovered.contains(&id.release)
        {
            // Actual aircraft decoding, coverage/policy checks and geoid conversion
            // remain mandatory in load before this identity can build a day store.
            recovered.push(id.release);
        }
    }
    recovered
}

/// The filename narrows the lookup; the embedded identity is authoritative.
pub(crate) fn path(dir: &Path, rel: &DayRelease, spec: &IngestSpec) -> PathBuf {
    let (_, bbox) = crate::fetch::bbox_key(&spec.bbox);
    dir.join("raw").join(format!(
        "{}{}_{bbox}.fdopraw.gz",
        rel.feed.tag_word(),
        if spec.include_jets { "+jets" } else { "" },
    ))
}

pub(crate) fn load(
    dir: &Path,
    rel: &DayRelease,
    spec: &IngestSpec,
    context: DayContext,
) -> Result<Option<(Vec<RawAircraft>, IngestStats)>, DataError> {
    let exact = path(dir, rel, spec);
    let mut first_error = match load_path(&exact, rel, spec, context, false) {
        Ok(Some(part)) => return Ok(Some(part)),
        Ok(None) => None,
        Err(e) => Some(e),
    };
    let entries = match std::fs::read_dir(dir.join("raw")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return first_error.map_or(Ok(None), Err);
        }
        Err(e) => return Err(first_error.unwrap_or_else(|| e.into())),
    };
    let feed = format!(
        "{}{}",
        rel.feed.tag_word(),
        if spec.include_jets { "+jets" } else { "" },
    );
    let expanded_feed = format!("{}+jets", rel.feed.tag_word());
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate == exact || !candidate.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".fdopraw.gz")
            && let Some((label, bbox)) = crate::fetch::parse_cache_name(&format!("{stem}.fdtrk"))
            && (label == feed || (!spec.include_jets && label == expanded_feed))
            && covers_bbox(bbox, spec.bbox)
        {
            candidates.push((label != feed, bbox.area_km2(), candidate));
        }
    }
    // Preserve the existing exact/covering ordinary preference before trying the
    // broader aircraft policy, even when its bbox is a closer fit.
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    for (from_expanded, _, candidate) in candidates {
        let cached_spec = IngestSpec {
            include_jets: spec.include_jets || from_expanded,
            ..*spec
        };
        match load_path(&candidate, rel, &cached_spec, context, true) {
            Ok(Some((mut aircraft, mut stats))) => {
                if from_expanded {
                    // The embedded identity must certify the expanded ingest. Drop
                    // jets before normal feed/type reconciliation and regional height
                    // correction, exactly as an ordinary fresh ingest would do.
                    let before = aircraft.len();
                    aircraft.retain(|a| a.engine != fdop_types::engine::EngineClass::Jet);
                    stats.jets_dropped += (before - aircraft.len()) as u64;
                    stats.kept_aircraft = aircraft.len() as u64;
                    stats.points_kept = aircraft.iter().map(|a| a.points.len() as u64).sum();
                }
                return Ok(Some((aircraft, stats)));
            }
            Ok(None) => {}
            Err(e) => {
                first_error
                    .get_or_insert_with(|| DataError(format!("{}: {e}", candidate.display())));
            }
        }
    }
    first_error.map_or(Ok(None), Err)
}

/// Reconcile complete local feeds with the ordinary day builder. When a processed
/// main-only cache already exists, `upgrade_main` requires that same main feed and
/// MLAT before replacing it; an incomplete raw set must leave the usable cache alone.
pub(crate) fn rebuild_day(
    dir: &Path,
    releases: &[DayRelease],
    spec: &IngestSpec,
    context: DayContext,
    include_mlat: bool,
    upgrade_main: Option<Feed>,
    mut unreadable: impl FnMut(&DayRelease, &DataError),
) -> Option<(Feed, bool, TrackStore)> {
    if upgrade_main.is_some_and(|feed| !include_mlat || !matches!(feed, Feed::Prod | Feed::Staging))
    {
        return None;
    }
    let day = fdop_types::time::civil_from_days((context.day_start / 86_400.0).floor() as i64);
    let mut parts = Vec::new();
    let mut tags = Vec::new();
    let mut main = None;
    let mut mlat = false;
    for feed in [Feed::Prod, Feed::Staging, Feed::Mlatonly] {
        if (feed == Feed::Mlatonly && (!include_mlat || main.is_none()))
            || (feed != Feed::Mlatonly
                && (main.is_some() || upgrade_main.is_some_and(|required| feed != required)))
        {
            continue;
        }
        for rel in releases
            .iter()
            .filter(|rel| rel.feed == feed && crate::fetch::release_for_day(rel, day))
        {
            match load(dir, rel, spec, context) {
                Ok(Some(part)) => {
                    parts.push(part);
                    tags.push(rel.tag.clone());
                    if feed == Feed::Mlatonly {
                        mlat = true;
                    } else {
                        main = Some(feed);
                    }
                    break;
                }
                Ok(None) => {}
                Err(error) => unreadable(rel, &error),
            }
        }
    }
    let main = main?;
    if upgrade_main.is_some() && !mlat {
        return None;
    }
    tags.sort();
    Some((
        main,
        mlat,
        crate::build::build_day_store(parts, spec, tags, context),
    ))
}

fn covers_bbox(outer: BBox, inner: BBox) -> bool {
    outer
        .as_array()
        .into_iter()
        .chain(inner.as_array())
        .all(f64::is_finite)
        && inner.south <= inner.north
        && inner.west <= inner.east
        && outer.south >= -90.0
        && outer.north <= 90.0
        && outer.west >= -180.0
        && outer.east <= 180.0
        && outer.south <= inner.south
        && outer.west <= inner.west
        && outer.north >= inner.north
        && outer.east >= inner.east
}

fn load_path(
    path: &Path,
    rel: &DayRelease,
    spec: &IngestSpec,
    context: DayContext,
    covering: bool,
) -> Result<Option<(Vec<RawAircraft>, IngestStats)>, DataError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // from_reader consumes the complete JSON; the multi-member decoder also checks
    // the gzip footer and rejects truncated/trailing data before a part is trusted.
    let mut cached: Checkpoint = serde_json::from_reader(BufReader::new(MultiGzDecoder::new(file)))
        .map_err(|e| DataError(format!("raw feed checkpoint: {e}")))?;
    let old_geoid = f64::from_bits(cached.identity.filter_bits[6]);
    if !old_geoid.is_finite() || !spec.geoid_n_m.is_finite() {
        return Ok(None);
    }
    let mut required = Identity::new(rel, spec, context);
    if covering {
        let bits = &cached.identity.filter_bits;
        let bbox = BBox::new(
            f64::from_bits(bits[0]),
            f64::from_bits(bits[1]),
            f64::from_bits(bits[2]),
            f64::from_bits(bits[3]),
        );
        if !covers_bbox(bbox, spec.bbox) {
            return Ok(None);
        }
        // Retain the whole raw part: cropping before feed reconciliation could
        // discard altitude offsets or leg boundaries. Final store filtering still
        // applies the requested region; the filename alone certifies no coverage.
        required.filter_bits[..4].copy_from_slice(&cached.identity.filter_bits[..4]);
    }
    if cached.identity != required {
        // The initial run may have used the default geoid before an NGS lookup
        // succeeded. All coverage, release and policy fields must still match;
        // only the reference of stored GNSS heights can be changed without re-ingest.
        required.filter_bits[6] = cached.identity.filter_bits[6];
        if cached.identity != required {
            return Ok(None);
        }
        // MSL = ellipsoid - N, so N_old - N_new has the same sign as the existing
        // processed-store conversion. Barometric heights and missing-value bits stay.
        let shift = (old_geoid - spec.geoid_n_m) as f32;
        if !shift.is_finite() {
            return Ok(None);
        }
        if shift != 0.0 {
            for aircraft in &mut cached.aircraft {
                for point in &mut aircraft.points {
                    if point.alt_geom_msl.is_finite() {
                        let height = point.alt_geom_msl + shift;
                        if !height.is_finite() {
                            return Ok(None);
                        }
                        point.alt_geom_msl = height;
                    }
                }
            }
        }
    }
    crate::ingest::validate_trace_counts(cached.stats.members, cached.stats.parse_errors)?;
    Ok(Some((cached.aircraft, cached.stats)))
}

pub(crate) fn save(
    dir: &Path,
    rel: &DayRelease,
    spec: &IngestSpec,
    context: DayContext,
    part: &(Vec<RawAircraft>, IngestStats),
) -> Result<(), DataError> {
    crate::ingest::validate_trace_counts(part.1.members, part.1.parse_errors)?;
    let path = path(dir, rel, spec);
    std::fs::create_dir_all(path.parent().expect("raw checkpoint has a parent"))?;
    // A unique sibling prevents concurrent ingests from sharing a temporary file.
    // Only a completely serialized, flushed gzip replaces the visible checkpoint.
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let (temporary, file) = loop {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("tmp-{}-{id}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let identity = Identity::new(rel, spec, context);
    let payload = CheckpointRef {
        identity: &identity,
        aircraft: &part.0,
        stats: &part.1,
    };
    let mut encoder = GzEncoder::new(BufWriter::new(file), Compression::fast());
    serde_json::to_writer(&mut encoder, &payload)
        .map_err(|e| DataError(format!("raw feed checkpoint: {e}")))?;
    let mut writer = encoder.finish()?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    std::fs::rename(temporary, path)?;
    Ok(())
}

// Integer encodings preserve missing-altitude NaNs (including their payloads), signed
// zero, and every finite float exactly. JSON float parsing is not a bit-exact contract.
pub(crate) mod f64_bits {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.to_bits())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        u64::deserialize(deserializer).map(f64::from_bits)
    }
}

pub(crate) mod f32_bits {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(value.to_bits())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
        u32::deserialize(deserializer).map(f32::from_bits)
    }
}

/// BoundaryEvents also lives in day-store metadata; override only its raw checkpoint
/// encoding so existing .fdtrk metadata stays compatible.
pub(crate) mod boundary_bits {
    use crate::build::BoundaryEvents;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        value: &BoundaryEvents,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let ground: Vec<_> = value
            .ground
            .iter()
            .map(|(a, b)| [a.to_bits(), b.to_bits()])
            .collect();
        let new_leg: Vec<_> = value.new_leg.iter().map(|t| t.to_bits()).collect();
        (ground, new_leg).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BoundaryEvents, D::Error> {
        let (ground, new_leg) = <(Vec<[u64; 2]>, Vec<u64>)>::deserialize(deserializer)?;
        Ok(BoundaryEvents {
            ground: ground
                .into_iter()
                .map(|[a, b]| (f64::from_bits(a), f64::from_bits(b)))
                .collect(),
            new_leg: new_leg.into_iter().map(f64::from_bits).collect(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::adsblol::{AssetPart, Feed};
    use crate::build::BoundaryEvents;
    use crate::ingest::RawPoint;
    use fdop_types::BBox;
    use fdop_types::engine::EngineClass;

    fn fixture() -> (PathBuf, DayRelease, IngestSpec, DayContext) {
        static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fdop-raw-cache-{}-{nonce}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let rel = DayRelease {
            feed: Feed::Prod,
            repo: "adsblol/globe_history_2026".into(),
            tag: "v2026.09.08-planes-readsb-prod-0".into(),
            parts: ["aa", "ab"]
                .into_iter()
                .map(|suffix| AssetPart {
                    name: format!("fixture.tar.{suffix}"),
                    url: format!("https://example.invalid/fixture.tar.{suffix}"),
                    size: 1024,
                })
                .collect(),
        };
        let spec = IngestSpec {
            bbox: BBox::new(0.0, 0.0, 1.0, 1.0),
            t_start: 1_788_825_600.0,
            t_end: 1_788_912_000.0,
            geoid_n_m: -32.0,
            keep_unknown: true,
            include_jets: false,
        };
        let context = DayContext::new(spec.t_start, 300.0, 3600.0);
        (dir, rel, spec, context)
    }

    fn part() -> (Vec<RawAircraft>, IngestStats) {
        let t = f64::from_bits(1_788_825_600.0_f64.to_bits() + 1);
        (
            vec![RawAircraft {
                icao: "000001".into(),
                reg: String::new(),
                typ: "PC12".into(),
                engine: EngineClass::Turboprop,
                feed: "prod".into(),
                points: vec![RawPoint {
                    t,
                    lat: -0.0,
                    lon: f64::from_bits(1),
                    alt_geom_msl: f32::from_bits(0x7fc1_2345),
                    alt_baro_m: f32::from_bits(0xffc5_4321),
                    gs_mps: -0.0,
                    track_deg: f32::INFINITY,
                    vrate_mps: f32::from_bits(0x3eaa_aaab),
                    flags: 0x5a,
                    new_leg: true,
                    retained_break: true,
                    retained_end: true,
                }],
                leg_boundaries: BoundaryEvents {
                    ground: vec![(t, f64::from_bits(t.to_bits() + 1))],
                    new_leg: vec![-0.0, f64::from_bits(t.to_bits() + 3)],
                },
            }],
            IngestStats {
                bytes: 2048,
                members: 1,
                aircraft_overlap: 1,
                kept_aircraft: 1,
                points_kept: 1,
                ..Default::default()
            },
        )
    }

    pub(crate) struct RebuildFixture {
        pub cache: PathBuf,
        pub dir: PathBuf,
        pub partial_path: PathBuf,
        pub want: IngestSpec,
        pub spec: IngestSpec,
        pub context: DayContext,
        pub releases: [DayRelease; 2],
        pub parts: [(Vec<RawAircraft>, IngestStats); 2],
    }

    impl RebuildFixture {
        pub fn checkpoint(&self, index: usize) {
            save(
                &self.dir,
                &self.releases[index],
                &self.spec,
                self.context,
                &self.parts[index],
            )
            .unwrap();
        }

        pub fn complete(&self) -> TrackStore {
            let mut tags: Vec<_> = self.releases.iter().map(|r| r.tag.clone()).collect();
            tags.sort();
            crate::build::build_day_store(self.parts.to_vec(), &self.spec, tags, self.context)
        }
    }

    /// One straight main-feed aircraft plus a second flight available only in MLAT.
    /// The shared raw GNSS/barometric pairs also recover the main flight's type and height.
    pub(crate) fn rebuild_fixture(main: Feed, include_jets: bool) -> RebuildFixture {
        use crate::ingest::ingest_day_reader;
        use crate::ingest::tests::{T0, gz, spec, tar_of};
        use fdop_types::geo::Enu;

        let (cache, _, _, _) = fixture();
        let dir = cache.join("adsb/2026-09-08");
        let want = IngestSpec {
            include_jets,
            ..spec()
        };
        let spec = IngestSpec {
            bbox: crate::fetch::bbox_key(&want.bbox).0,
            t_start: T0 - 3600.0,
            t_end: T0 + 86_400.0 + 3600.0,
            ..want
        };
        let context = DayContext::new(T0, 300.0, 3600.0);
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let trace = |second: bool, gnss: bool| {
            let rows: Vec<_> = (0..=8)
                .map(|i| {
                    let dt = 10.0 * i as f64 + if gnss && !second { 5.0 } else { 0.0 };
                    let (east, north, height, speed, heading) = if second {
                        (900.0, 85.0 * (dt - 40.0), 1200.0, 85.0, 0.0)
                    } else {
                        (130.0 * (dt - 40.0), 800.0, 1500.0, 130.0, 90.0)
                    };
                    let (lat, lon, altitude) = enu.to_geodetic([east, north, height]);
                    serde_json::json!([
                        70_000.0 + if second { 200.0 } else { 0.0 } + dt,
                        lat,
                        lon,
                        (altitude - 100.0) / 0.3048,
                        speed / 0.514444,
                        heading,
                        0,
                        0,
                        null,
                        if gnss { "mlat" } else { "adsb_icao" },
                        gnss.then_some((altitude + spec.geoid_n_m) / 0.3048),
                        0,
                        null,
                        null
                    ])
                })
                .collect();
            serde_json::json!({
                "icao": if second { "d4e5f6" } else { "a1b2c3" },
                "t": if second { "C172" } else if !gnss { "" }
                    else if include_jets { "B738" } else { "PC12" },
                "timestamp": T0, "trace": rows
            })
            .to_string()
        };
        let archives = [
            tar_of(&[("trace_full_a1b2c3.json", gz(&trace(false, false)))]),
            tar_of(&[
                ("trace_full_a1b2c3.json", gz(&trace(false, true))),
                ("trace_full_d4e5f6.json", gz(&trace(true, true))),
            ]),
        ];
        let releases = std::array::from_fn(|i| {
            let feed = if i == 0 { main } else { Feed::Mlatonly };
            let tag = crate::adsblol::tag_for(2026, 9, 8, feed, 0);
            DayRelease {
                feed,
                repo: crate::adsblol::repo_for_year(2026),
                parts: vec![AssetPart {
                    name: format!("{tag}.tar"),
                    url: format!("https://example.invalid/{tag}.tar"),
                    size: archives[i].len() as u64,
                }],
                tag,
            }
        });
        let parts = std::array::from_fn(|i| {
            ingest_day_reader(
                archives[i].as_slice(),
                releases[i].feed.label(),
                &spec,
                &|_| {},
                context,
            )
            .unwrap()
        });
        let partial_path = dir.join(format!(
            "{}{}_{}.fdtrk",
            main.tag_word(),
            if include_jets { "+jets" } else { "" },
            crate::fetch::bbox_key(&spec.bbox).1
        ));
        crate::build::build_day_store(
            vec![parts[0].clone()],
            &spec,
            vec![releases[0].tag.clone()],
            context,
        )
        .save(&partial_path)
        .unwrap();
        std::fs::write(
            dir.join("releases.json"),
            serde_json::to_vec(&releases).unwrap(),
        )
        .unwrap();
        RebuildFixture {
            cache,
            dir,
            partial_path,
            want,
            spec,
            context,
            releases,
            parts,
        }
    }

    #[test]
    fn complete_raw_pair_recovers_aircraft_hidden_by_a_processed_main_cache() {
        use fdop_types::store::alt_src;

        for main in [Feed::Prod, Feed::Staging] {
            for include_jets in [false, true] {
                let f = rebuild_fixture(main, include_jets);
                f.checkpoint(0);
                f.checkpoint(1);
                let before = std::fs::read(&f.partial_path).unwrap();
                let old = TrackStore::load(&f.partial_path).unwrap();
                assert_eq!(old.legs.len(), 1);
                assert_eq!(old.legs[0].icao, "a1b2c3");
                assert_eq!(old.legs[0].engine, EngineClass::Unknown);
                assert!(
                    old.legs[0]
                        .points
                        .iter()
                        .all(|p| p.alt_src == alt_src::BARO_RAW)
                );
                let (selected, mlat, mut rebuilt) = rebuild_day(
                    &f.dir,
                    &f.releases,
                    &f.spec,
                    f.context,
                    true,
                    Some(main),
                    |_, error| panic!("valid completed raw feed: {error}"),
                )
                .unwrap();
                assert_eq!(selected, main);
                assert!(mlat);
                assert_eq!(rebuilt.legs.len(), 2);
                assert!(
                    rebuilt
                        .legs
                        .iter()
                        .any(|l| l.icao == "d4e5f6" && l.engine == EngineClass::Piston)
                );
                let identified = rebuilt.legs.iter().find(|l| l.icao == "a1b2c3").unwrap();
                assert_eq!(identified.engine == EngineClass::Jet, include_jets);
                assert_ne!(identified.engine, EngineClass::Unknown);
                assert!(
                    identified
                        .points
                        .iter()
                        .any(|p| p.alt_src == alt_src::BARO_CORRECTED)
                );
                // The recovered output is exactly the ordinary joint raw build, not a
                // processed-store append that cannot reconstruct altitude calibration.
                let mut reference = f.complete();
                rebuilt.meta.created_unix = 0.0;
                reference.meta.created_unix = 0.0;
                let output = f.dir.join("reconstructed.fdtrk");
                let expected = f.dir.join("joint-reference.fdtrk");
                rebuilt.save(&output).unwrap();
                reference.save(&expected).unwrap();
                assert_eq!(
                    std::fs::read(&output).unwrap(),
                    std::fs::read(&expected).unwrap()
                );
                let roundtrip = f.dir.join("roundtrip.fdtrk");
                TrackStore::load(&output).unwrap().save(&roundtrip).unwrap();
                assert_eq!(
                    std::fs::read(&roundtrip).unwrap(),
                    std::fs::read(&output).unwrap()
                );
                assert_eq!(std::fs::read(&f.partial_path).unwrap(), before);
            }
        }
    }

    #[test]
    fn raw_upgrade_requires_both_compatible_feeds_and_the_selected_main() {
        for problem in [
            "missing main",
            "missing MLAT",
            "corrupt MLAT",
            "release identity",
            "wrong day",
            "context",
            "time filter",
            "jet policy",
            "other main",
        ] {
            let mut f = rebuild_fixture(Feed::Prod, false);
            let before = std::fs::read(&f.partial_path).unwrap();
            if problem == "wrong day" {
                f.releases[0].tag = crate::adsblol::tag_for(2026, 9, 9, Feed::Prod, 0);
            }
            if problem != "missing main" {
                f.checkpoint(0);
            }
            if problem != "missing MLAT" {
                f.checkpoint(1);
            }
            match problem {
                "corrupt MLAT" => {
                    // Retain the original fixture before replacing it with an interrupted file.
                    let checkpoint = path(&f.dir, &f.releases[1], &f.spec);
                    std::fs::copy(&checkpoint, f.dir.join("complete-mlat.backup")).unwrap();
                    std::fs::write(checkpoint, b"incomplete raw checkpoint").unwrap();
                }
                "release identity" => f.releases[1].parts[0].size += 1,
                "context" => f.context = DayContext::new(f.context.day_start, 301.0, 3600.0),
                "time filter" => f.spec.t_start += 1.0,
                "jet policy" => f.spec.include_jets = true,
                _ => {}
            }
            let required = if problem == "other main" {
                Feed::Staging
            } else {
                Feed::Prod
            };
            let mut errors = Vec::new();
            assert!(
                rebuild_day(
                    &f.dir,
                    &f.releases,
                    &f.spec,
                    f.context,
                    true,
                    Some(required),
                    |rel, error| errors.push((rel.feed, error.to_string())),
                )
                .is_none(),
                "{problem} must leave the processed fallback selected"
            );
            assert_eq!(errors.len(), usize::from(problem == "corrupt MLAT"));
            assert_eq!(std::fs::read(&f.partial_path).unwrap(), before);
            assert_eq!(TrackStore::load(&f.partial_path).unwrap().legs.len(), 1);
        }
    }

    #[test]
    fn raw_reconstruction_keeps_main_only_policy_without_an_upgrade() {
        for main in [Feed::Prod, Feed::Staging] {
            let f = rebuild_fixture(main, false);
            f.checkpoint(0);
            // A first reconstruction still preserves the completed main feed if MLAT
            // is missing. The stronger requirement applies only to replacing a cache.
            let (selected, mlat, rebuilt) = rebuild_day(
                &f.dir,
                &f.releases,
                &f.spec,
                f.context,
                true,
                None,
                |_, error| panic!("valid main feed: {error}"),
            )
            .unwrap();
            assert_eq!(selected, main);
            assert!(!mlat);
            assert_eq!(rebuilt.legs.len(), 1);
            f.checkpoint(1);
            let (selected, mlat, rebuilt) = rebuild_day(
                &f.dir,
                &f.releases,
                &f.spec,
                f.context,
                false,
                None,
                |_, error| panic!("valid main feed: {error}"),
            )
            .unwrap();
            assert_eq!(selected, main);
            assert!(!mlat);
            assert_eq!(rebuilt.legs.len(), 1);
            assert!(
                rebuild_day(
                    &f.dir,
                    &f.releases,
                    &f.spec,
                    f.context,
                    false,
                    Some(main),
                    |_, _| {},
                )
                .is_none()
            );
        }
    }

    #[test]
    fn checkpoint_preserves_float_bits_boundaries_metadata_and_empty_success() {
        let (dir, rel, spec, context) = fixture();
        assert!(load(&dir, &rel, &spec, context).unwrap().is_none());
        let original = part();
        save(&dir, &rel, &spec, context, &original).unwrap();
        let restored = load(&dir, &rel, &spec, context).unwrap().unwrap();
        assert_eq!(restored.1, original.1);
        let (a, b) = (&original.0[0], &restored.0[0]);
        assert_eq!(
            (&a.icao, &a.reg, &a.typ, a.engine, &a.feed),
            (&b.icao, &b.reg, &b.typ, b.engine, &b.feed)
        );
        let (a_point, b_point) = (a.points[0], b.points[0]);
        assert_eq!(
            [a_point.t, a_point.lat, a_point.lon].map(f64::to_bits),
            [b_point.t, b_point.lat, b_point.lon].map(f64::to_bits)
        );
        assert_eq!(
            [
                a_point.alt_geom_msl,
                a_point.alt_baro_m,
                a_point.gs_mps,
                a_point.track_deg,
                a_point.vrate_mps,
            ]
            .map(f32::to_bits),
            [
                b_point.alt_geom_msl,
                b_point.alt_baro_m,
                b_point.gs_mps,
                b_point.track_deg,
                b_point.vrate_mps,
            ]
            .map(f32::to_bits)
        );
        assert_eq!(
            (
                a_point.flags,
                a_point.new_leg,
                a_point.retained_break,
                a_point.retained_end
            ),
            (
                b_point.flags,
                b_point.new_leg,
                b_point.retained_break,
                b_point.retained_end
            )
        );
        let ground_bits = |events: &BoundaryEvents| {
            events
                .ground
                .iter()
                .map(|(lo, hi)| (lo.to_bits(), hi.to_bits()))
                .collect::<Vec<_>>()
        };
        let leg_bits = |events: &BoundaryEvents| {
            events
                .new_leg
                .iter()
                .map(|t| t.to_bits())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ground_bits(&a.leg_boundaries),
            ground_bits(&b.leg_boundaries)
        );
        assert_eq!(leg_bits(&a.leg_boundaries), leg_bits(&b.leg_boundaries));

        // Missing data and a completely ingested empty feed are distinct states.
        let empty = (
            Vec::new(),
            IngestStats {
                bytes: 1024,
                ..Default::default()
            },
        );
        save(&dir, &rel, &spec, context, &empty).unwrap();
        let restored = load(&dir, &rel, &spec, context).unwrap().unwrap();
        assert!(restored.0.is_empty());
        assert_eq!(restored.1, empty.1);
    }

    #[test]
    fn recovered_release_metadata_never_replaces_listed_assets_or_main_feed() {
        let f = rebuild_fixture(Feed::Prod, false);
        f.checkpoint(0);
        f.checkpoint(1);
        assert!(recovery_releases(&f.dir, &f.releases, f.context, true).is_empty());
        assert_eq!(
            recovery_releases(&f.dir, &[], f.context, false),
            vec![f.releases[0].clone()]
        );
        for other_main in [false, true] {
            let mut listed_main = f.releases[0].clone();
            if other_main {
                listed_main.feed = Feed::Staging;
                listed_main.tag = crate::adsblol::tag_for(2026, 9, 8, Feed::Staging, 0);
                listed_main.parts[0].name = format!("{}.tar", listed_main.tag);
                listed_main.parts[0].url =
                    format!("https://example.invalid/{}.tar", listed_main.tag);
            } else {
                listed_main.parts[0].size += 1;
            }
            let mut releases = vec![listed_main];
            let recovered = recovery_releases(&f.dir, &releases, f.context, true);
            assert_eq!(recovered, vec![f.releases[1].clone()]);
            releases.extend(recovered);
            assert!(
                rebuild_day(
                    &f.dir,
                    &releases,
                    &f.spec,
                    f.context,
                    true,
                    None,
                    |_, error| panic!("valid checkpoint with a different identity: {error}"),
                )
                .is_none(),
                "embedded metadata must not override a listed main identity"
            );
        }
    }

    #[test]
    fn embedded_release_metadata_requires_a_completed_certified_checkpoint() {
        for problem in [
            "none",
            "version",
            "context",
            "unknown policy",
            "jet label",
            "feed",
            "day",
            "repository",
            "no assets",
            "empty asset",
            "mixed assets",
            "missing aircraft",
            "missing stats",
            "aircraft shape",
            "stats shape",
            "all traces unreadable",
            "truncated",
            "trailing",
            "temporary",
            "directory",
        ] {
            let f = rebuild_fixture(Feed::Prod, false);
            let mut identity = Identity::new(&f.releases[0], &f.spec, f.context);
            match problem {
                "version" => identity.version += 1,
                "context" => identity.context_bits[0] += 1,
                "unknown policy" => identity.keep_unknown = false,
                "jet label" => identity.include_jets = true,
                "feed" => identity.release.feed = Feed::Staging,
                "day" => {
                    identity.release.tag = crate::adsblol::tag_for(2026, 9, 9, Feed::Prod, 0);
                    identity.release.parts[0].name = format!("{}.tar", identity.release.tag);
                }
                "repository" => identity.release.repo.push_str("-other"),
                "no assets" => identity.release.parts.clear(),
                "empty asset" => identity.release.parts[0].size = 0,
                "mixed assets" => {
                    let mut split = identity.release.parts[0].clone();
                    split.name.push_str(".aa");
                    identity.release.parts.push(split);
                }
                _ => {}
            }
            let mut payload = serde_json::to_value(CheckpointRef {
                identity: &identity,
                aircraft: &f.parts[0].0,
                stats: &f.parts[0].1,
            })
            .unwrap();
            match problem {
                "missing aircraft" => {
                    payload.as_object_mut().unwrap().remove("aircraft");
                }
                "missing stats" => {
                    payload.as_object_mut().unwrap().remove("stats");
                }
                "aircraft shape" => payload["aircraft"] = serde_json::json!({}),
                "stats shape" => payload["stats"] = serde_json::json!([]),
                "all traces unreadable" => {
                    payload["aircraft"] = serde_json::json!([]);
                    payload["stats"]["parse_errors"] = payload["stats"]["members"].clone();
                }
                _ => {}
            }
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            serde_json::to_writer(&mut encoder, &payload).unwrap();
            let mut bytes = encoder.finish().unwrap();
            if problem == "truncated" {
                bytes.pop();
            }
            if problem == "trailing" {
                bytes.extend_from_slice(b"unfinished next gzip");
            }
            let final_path = path(&f.dir, &f.releases[0], &f.spec);
            std::fs::create_dir_all(final_path.parent().unwrap()).unwrap();
            if problem == "directory" {
                std::fs::create_dir_all(&final_path).unwrap();
            } else {
                let path = if problem == "temporary" {
                    final_path.with_extension("tmp-123-0")
                } else {
                    final_path
                };
                std::fs::write(path, bytes).unwrap();
            }
            if problem == "all traces unreadable" {
                assert!(load(&f.dir, &f.releases[0], &f.spec, f.context).is_err());
            }
            let recovered = recovery_releases(&f.dir, &[], f.context, true);
            if problem == "none" {
                assert_eq!(recovered, vec![f.releases[0].clone()]);
                let (main, mlat, rebuilt) = rebuild_day(
                    &f.dir,
                    &recovered,
                    &f.spec,
                    f.context,
                    true,
                    None,
                    |_, error| panic!("valid checkpoint: {error}"),
                )
                .unwrap();
                assert_eq!(main, Feed::Prod);
                assert!(!mlat);
                assert!(!rebuilt.legs.is_empty());
            } else {
                assert!(recovered.is_empty(), "{problem}");
            }
        }
    }

    #[test]
    fn checkpoint_identity_requires_exact_release_coverage_policy_and_context() {
        let (dir, rel, spec, context) = fixture();
        save(&dir, &rel, &spec, context, &part()).unwrap();
        let original_path = path(&dir, &rel, &spec);
        let changed_specs: [fn(&mut IngestSpec); 8] = [
            |s| s.bbox.south = f64::from_bits(s.bbox.south.to_bits() + 1),
            |s| s.bbox.west = f64::from_bits(s.bbox.west.to_bits() + 1),
            |s| s.bbox.north = f64::from_bits(s.bbox.north.to_bits() + 1),
            |s| s.bbox.east = f64::from_bits(s.bbox.east.to_bits() + 1),
            |s| s.t_start = f64::from_bits(s.t_start.to_bits() + 1),
            |s| s.t_end = f64::from_bits(s.t_end.to_bits() + 1),
            |s| s.keep_unknown = !s.keep_unknown,
            |s| s.include_jets = !s.include_jets,
        ];
        for change in changed_specs {
            let mut changed = spec;
            change(&mut changed);
            let renamed = path(&dir, &rel, &changed);
            if renamed != original_path {
                std::fs::copy(&original_path, &renamed).unwrap();
            }
            assert!(load(&dir, &rel, &changed, context).unwrap().is_none());
        }
        let changed_releases: [fn(&mut DayRelease); 8] = [
            |r| r.feed = Feed::Staging,
            |r| r.repo.push_str("-other"),
            |r| r.tag.push_str("-other"),
            |r| r.parts[0].name.push_str("-other"),
            |r| r.parts[0].url.push_str("-other"),
            |r| r.parts[0].size += 1,
            |r| {
                r.parts.pop();
            },
            |r| r.parts.reverse(),
        ];
        for change in changed_releases {
            let mut changed = rel.clone();
            change(&mut changed);
            let renamed = path(&dir, &changed, &spec);
            if renamed != original_path {
                std::fs::copy(&original_path, &renamed).unwrap();
            }
            assert!(load(&dir, &changed, &spec, context).unwrap().is_none());
        }
        for changed in [
            DayContext::new(f64::from_bits(spec.t_start.to_bits() + 1), 300.0, 3600.0),
            DayContext::new(
                spec.t_start,
                f64::from_bits(300.0_f64.to_bits() + 1),
                3600.0,
            ),
            DayContext::new(
                spec.t_start,
                300.0,
                f64::from_bits(3600.0_f64.to_bits() + 1),
            ),
        ] {
            assert!(load(&dir, &rel, &spec, changed).unwrap().is_none());
        }
        assert!(load(&dir, &rel, &spec, context).unwrap().is_some());
    }

    #[test]
    fn certified_jet_checkpoint_recovers_ordinary_reports_without_changing_priority() {
        let (dir, rel, spec, context) = fixture();
        let want = IngestSpec {
            bbox: BBox::new(0.2, 0.2, 0.4, 0.4),
            geoid_n_m: -30.0,
            ..spec
        };
        let expanded = IngestSpec {
            bbox: want.bbox,
            include_jets: true,
            ..spec
        };
        let mut ordinary = part();
        let sample = ordinary.0[0].points[0];
        ordinary.0[0].points.push(RawPoint {
            alt_geom_msl: 880.0,
            ..sample
        });
        ordinary.1.points_kept = 2;
        let mut complete = ordinary.clone();
        let mut jet = ordinary.0[0].clone();
        jet.icao = "000002".into();
        jet.typ = "B738".into();
        jet.engine = EngineClass::Jet;
        complete.0.push(jet);
        complete.1.members += 1;
        complete.1.aircraft_overlap += 1;
        complete.1.kept_aircraft += 1;
        complete.1.points_kept += 2;
        save(&dir, &rel, &expanded, context, &complete).unwrap();
        let expanded_path = path(&dir, &rel, &expanded);
        let bytes = std::fs::read(&expanded_path).unwrap();
        let restored = load(&dir, &rel, &want, context).unwrap().unwrap();
        let mut expected = ordinary.clone();
        expected.0[0].points[1].alt_geom_msl = 878.0;
        expected.1.members += 1;
        expected.1.aircraft_overlap += 1;
        expected.1.jets_dropped = 1;
        assert_eq!(restored.1, expected.1);
        assert_eq!(
            serde_json::to_vec(&restored.0).unwrap(),
            serde_json::to_vec(&expected.0).unwrap()
        );
        assert_eq!(std::fs::read(&expanded_path).unwrap(), bytes);

        // A renamed ordinary checkpoint is not an expanded-ingest certificate.
        let source = dir.join("ordinary-source");
        save(&source, &rel, &want, context, &ordinary).unwrap();
        std::fs::copy(path(&source, &rel, &want), &expanded_path).unwrap();
        assert!(load(&dir, &rel, &want, context).unwrap().is_none());
        assert!(load(&source, &rel, &expanded, context).unwrap().is_none());
        std::fs::write(&expanded_path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(load(&dir, &rel, &want, context).is_err());
        std::fs::write(&expanded_path, &bytes).unwrap();
        let wrong_time = IngestSpec {
            t_end: want.t_end + 1.0,
            ..want
        };
        assert!(load(&dir, &rel, &wrong_time, context).unwrap().is_none());

        // Even the exact expanded bbox cannot displace a usable wider ordinary
        // checkpoint; the exact ordinary checkpoint still takes priority over both.
        ordinary.1.bytes += 1;
        save(&dir, &rel, &spec, context, &ordinary).unwrap();
        let restored = load(&dir, &rel, &want, context).unwrap().unwrap();
        assert_eq!(restored.1, ordinary.1);
        ordinary.1.bytes += 1;
        save(&dir, &rel, &want, context, &ordinary).unwrap();
        let restored = load(&dir, &rel, &want, context).unwrap().unwrap();
        assert_eq!(restored.1, ordinary.1);
    }

    #[test]
    fn covering_checkpoint_keeps_raw_evidence_and_prefers_exact_then_smallest() {
        let (dir, rel, spec, context) = fixture();
        let want = IngestSpec {
            bbox: BBox::new(0.2, 0.2, 0.4, 0.4),
            geoid_n_m: -30.0,
            ..spec
        };
        let smaller = IngestSpec {
            bbox: BBox::new(0.1, 0.1, 0.6, 0.6),
            ..spec
        };
        let mut broad = part();
        let sample = broad.0[0].points[0];
        broad.0[0].points.push(RawPoint {
            alt_geom_msl: 880.0,
            ..sample
        });
        let mut narrow = broad.clone();
        narrow.1.bytes += 1;
        // The wider checkpoint was created first, but the tighter coverage wins.
        save(&dir, &rel, &spec, context, &broad).unwrap();
        save(&dir, &rel, &smaller, context, &narrow).unwrap();
        let original = std::fs::read(path(&dir, &rel, &smaller)).unwrap();
        let restored = load(&dir, &rel, &want, context).unwrap().unwrap();
        let mut expected = narrow.clone();
        expected.0[0].points[1].alt_geom_msl = 878.0;
        assert_eq!(restored.1, expected.1);
        // Preserve every original report and boundary, including outside the query;
        // the day builder still needs their altitude and continuity evidence.
        assert_eq!(
            serde_json::to_vec(&restored.0).unwrap(),
            serde_json::to_vec(&expected.0).unwrap()
        );
        assert_eq!(std::fs::read(path(&dir, &rel, &smaller)).unwrap(), original);

        let mut exact = part();
        exact.1.bytes += 2;
        save(&dir, &rel, &want, context, &exact).unwrap();
        let restored = load(&dir, &rel, &want, context).unwrap().unwrap();
        assert_eq!(restored.1, exact.1);
        assert_eq!(
            serde_json::to_vec(&restored.0).unwrap(),
            serde_json::to_vec(&exact.0).unwrap()
        );
    }

    #[test]
    fn covering_checkpoint_requires_certified_bbox_and_exact_nonspatial_identity() {
        for case in [
            "south",
            "west",
            "north",
            "east",
            "nonfinite",
            "time_start",
            "time_end",
            "unknown",
            "jets",
            "feed",
            "release",
            "parts",
            "context",
            "version",
        ] {
            let (dir, rel, spec, context) = fixture();
            let want = IngestSpec {
                bbox: BBox::new(0.2, 0.2, 0.4, 0.4),
                geoid_n_m: -30.0,
                ..spec
            };
            let mut identity = Identity::new(&rel, &spec, context);
            match case {
                "south" => identity.filter_bits[0] = 0.3_f64.to_bits(),
                "west" => identity.filter_bits[1] = 0.3_f64.to_bits(),
                "north" => identity.filter_bits[2] = 0.3_f64.to_bits(),
                "east" => identity.filter_bits[3] = 0.3_f64.to_bits(),
                "nonfinite" => identity.filter_bits[0] = f64::NEG_INFINITY.to_bits(),
                "time_start" => identity.filter_bits[4] += 1,
                "time_end" => identity.filter_bits[5] += 1,
                "unknown" => identity.keep_unknown = false,
                "jets" => identity.include_jets = true,
                "feed" => identity.release.feed = Feed::Mlatonly,
                "release" => identity.release.parts[0].size += 1,
                "parts" => identity.release.parts.reverse(),
                "context" => identity.context_bits[2] += 1,
                "version" => identity.version = 0,
                _ => unreachable!(),
            }
            let part = part();
            let path = path(&dir, &rel, &spec);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::fast());
            serde_json::to_writer(
                &mut encoder,
                &CheckpointRef {
                    identity: &identity,
                    aircraft: &part.0,
                    stats: &part.1,
                },
            )
            .unwrap();
            encoder.finish().unwrap();
            // The filename claims coverage, but the embedded identity is decisive.
            assert!(
                load(&dir, &rel, &want, context).unwrap().is_none(),
                "{case}"
            );
        }
    }

    #[test]
    fn covering_checkpoint_skips_damaged_and_incompatible_candidates() {
        let (dir, rel, spec, context) = fixture();
        let want = IngestSpec {
            bbox: BBox::new(0.3, 0.3, 0.4, 0.4),
            ..spec
        };
        let original = part();
        save(&dir, &rel, &spec, context, &original).unwrap();
        let incompatible = IngestSpec {
            bbox: BBox::new(0.1, 0.1, 0.7, 0.7),
            t_end: spec.t_end + 1.0,
            ..spec
        };
        save(&dir, &rel, &incompatible, context, &original).unwrap();
        let corrupt = IngestSpec {
            bbox: BBox::new(0.2, 0.2, 0.6, 0.6),
            ..spec
        };
        save(&dir, &rel, &corrupt, context, &original).unwrap();
        let corrupt_path = path(&dir, &rel, &corrupt);
        let bytes = std::fs::read(&corrupt_path).unwrap();
        // Even a fully readable JSON payload is unusable without its gzip footer.
        std::fs::write(&corrupt_path, &bytes[..bytes.len() - 1]).unwrap();
        std::fs::write(path(&dir, &rel, &want), b"damaged exact checkpoint").unwrap();
        let restored = load(&dir, &rel, &want, context).unwrap().unwrap();
        assert_eq!(restored.1, original.1);
        assert_eq!(
            serde_json::to_vec(&restored.0).unwrap(),
            serde_json::to_vec(&original.0).unwrap()
        );
        std::fs::write(path(&dir, &rel, &spec), b"damaged covering checkpoint").unwrap();
        assert!(load(&dir, &rel, &want, context).is_err());
    }

    #[test]
    fn checkpoint_references_only_finite_gnss_heights_to_a_new_geoid() {
        let (dir, rel, spec, context) = fixture();
        let mut original = part();
        let sample = original.0[0].points[0];
        for (i, height) in [880.0, -0.0, -12.75].into_iter().enumerate() {
            original.0[0].points.push(RawPoint {
                t: sample.t + (i + 1) as f64,
                alt_geom_msl: height,
                alt_baro_m: 840.0,
                ..sample
            });
        }
        original.1.points_kept = original.0[0].points.len() as u64;
        save(&dir, &rel, &spec, context, &original).unwrap();
        let checkpoint = path(&dir, &rel, &spec);
        let bytes = std::fs::read(&checkpoint).unwrap();
        let original_bits = serde_json::to_vec(&original.0).unwrap();
        for geoid in [
            spec.geoid_n_m,
            -30.0,
            -35.5,
            f64::from_bits(spec.geoid_n_m.to_bits() + 1),
        ] {
            let requested = IngestSpec {
                geoid_n_m: geoid,
                ..spec
            };
            let got = load(&dir, &rel, &requested, context).unwrap().unwrap();
            let mut expected = original.0.clone();
            if geoid != spec.geoid_n_m {
                for point in &mut expected[0].points {
                    if point.alt_geom_msl.is_finite() {
                        point.alt_geom_msl =
                            (point.alt_geom_msl as f64 + spec.geoid_n_m - geoid) as f32;
                    }
                }
            }
            // The raw serde representation encodes every float by its bits, so this
            // also checks NaN payloads, barometric heights, motion and boundaries.
            assert_eq!(
                serde_json::to_vec(&got.0).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
            assert_eq!(got.1, original.1);
            assert_eq!(std::fs::read(&checkpoint).unwrap(), bytes);
            let unchanged = load(&dir, &rel, &spec, context).unwrap().unwrap();
            assert_eq!(serde_json::to_vec(&unchanged.0).unwrap(), original_bits);
        }

        // The exception must never hide a coverage mismatch that shares a filename.
        let changed = IngestSpec {
            t_end: spec.t_end + 1.0,
            geoid_n_m: -30.0,
            ..spec
        };
        assert!(load(&dir, &rel, &changed, context).unwrap().is_none());
    }

    #[test]
    fn checkpoint_rejects_invalid_geoid_conversions_without_changing_raw_data() {
        let (dir, rel, spec, context) = fixture();
        let original = part();
        save(&dir, &rel, &spec, context, &original).unwrap();
        for geoid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            let requested = IngestSpec {
                geoid_n_m: geoid,
                ..spec
            };
            assert!(load(&dir, &rel, &requested, context).unwrap().is_none());
        }
        for geoid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let invalid = IngestSpec {
                geoid_n_m: geoid,
                ..spec
            };
            save(&dir, &rel, &invalid, context, &original).unwrap();
            assert!(load(&dir, &rel, &spec, context).unwrap().is_none());
            assert!(load(&dir, &rel, &invalid, context).unwrap().is_none());
        }
        let mut extreme = original;
        extreme.0[0].points[0].alt_geom_msl = f32::MAX;
        save(&dir, &rel, &spec, context, &extreme).unwrap();
        let checkpoint = path(&dir, &rel, &spec);
        let bytes = std::fs::read(&checkpoint).unwrap();
        assert!(
            load(
                &dir,
                &rel,
                &IngestSpec {
                    geoid_n_m: -1e38,
                    ..spec
                },
                context,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(std::fs::read(&checkpoint).unwrap(), bytes);
    }

    #[test]
    fn checkpoint_rejects_old_versions_corruption_truncation_and_trailing_data() {
        let (dir, rel, spec, context) = fixture();
        let part = part();
        save(&dir, &rel, &spec, context, &part).unwrap();
        let path = path(&dir, &rel, &spec);
        let original = std::fs::read(&path).unwrap();
        for damaged in [
            original[..original.len() - 1].to_vec(),
            {
                let mut bytes = original.clone();
                let crc = bytes.len() - 8;
                bytes[crc] ^= 1;
                bytes
            },
            [original.as_slice(), b"trailing garbage"].concat(),
            b"not a gzip checkpoint".to_vec(),
        ] {
            std::fs::write(&path, damaged).unwrap();
            assert!(load(&dir, &rel, &spec, context).is_err());
        }

        for old_context in [false, true] {
            let mut identity = Identity::new(&rel, &spec, context);
            if old_context {
                identity.context_bits[0] = 0;
            } else {
                identity.version = 0;
            }
            let mut encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::fast());
            serde_json::to_writer(
                &mut encoder,
                &CheckpointRef {
                    identity: &identity,
                    aircraft: &part.0,
                    stats: &part.1,
                },
            )
            .unwrap();
            encoder.finish().unwrap();
            assert!(load(&dir, &rel, &spec, context).unwrap().is_none());
        }

        save(&dir, &rel, &spec, context, &part).unwrap();
        assert!(load(&dir, &rel, &spec, context).unwrap().is_some());
    }

    #[test]
    fn checkpoint_replacement_preserves_an_existing_readers_complete_file() {
        let (dir, rel, spec, context) = fixture();
        let first = part();
        save(&dir, &rel, &spec, context, &first).unwrap();
        let original = File::open(path(&dir, &rel, &spec)).unwrap();
        let mut next = part();
        next.1.bytes += 1;
        next.0[0].reg = "SYNTHETIC".into();
        save(&dir, &rel, &spec, context, &next).unwrap();

        let pinned: Checkpoint =
            serde_json::from_reader(BufReader::new(MultiGzDecoder::new(original))).unwrap();
        assert_eq!(pinned.stats, first.1);
        assert_eq!(pinned.aircraft[0].reg, first.0[0].reg);
        let current = load(&dir, &rel, &spec, context).unwrap().unwrap();
        assert_eq!(current.1, next.1);
        assert_eq!(current.0[0].reg, next.0[0].reg);
    }
}
