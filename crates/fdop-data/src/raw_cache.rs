//! Private checkpoints of completely ingested release parts. Retain raw altitude pairs
//! and boundary provenance so a later feed is reconciled by the ordinary day builder.
//! This is disposable cache data, not a public interchange format: a version, release,
//! coverage or policy mismatch is a miss and an unreadable file is refetched. A change
//! only to the geoid re-references GNSS heights without discarding the completed ingest.
//! After an exact miss, a covering spatial checkpoint can supply a narrower retry; all
//! other filter, policy, release and midnight-context fields must still match exactly.

use crate::DataError;
use crate::adsblol::DayRelease;
use crate::ingest::{DayContext, IngestSpec, IngestStats, RawAircraft};
use fdop_types::BBox;
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
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate == exact || !candidate.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".fdopraw.gz")
            && let Some((label, bbox)) = crate::fetch::parse_cache_name(&format!("{stem}.fdtrk"))
            && label == feed
            && covers_bbox(bbox, spec.bbox)
        {
            candidates.push((bbox.area_km2(), candidate));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, candidate) in candidates {
        match load_path(&candidate, rel, spec, context, true) {
            Ok(Some(part)) => return Ok(Some(part)),
            Ok(None) => {}
            Err(e) => {
                first_error
                    .get_or_insert_with(|| DataError(format!("{}: {e}", candidate.display())));
            }
        }
    }
    first_error.map_or(Ok(None), Err)
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
    Ok(Some((cached.aircraft, cached.stats)))
}

pub(crate) fn save(
    dir: &Path,
    rel: &DayRelease,
    spec: &IngestSpec,
    context: DayContext,
    part: &(Vec<RawAircraft>, IngestStats),
) -> Result<(), DataError> {
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
mod tests {
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
