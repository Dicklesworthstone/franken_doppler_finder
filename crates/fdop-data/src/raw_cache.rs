//! Private checkpoints of completely ingested release parts. Retain raw altitude pairs
//! and boundary provenance so a later feed is reconciled by the ordinary day builder.
//! This is disposable cache data, not a public interchange format: a version, release,
//! filter or policy mismatch is a miss and an unreadable file is refetched.

use crate::DataError;
use crate::adsblol::DayRelease;
use crate::ingest::{DayContext, IngestSpec, IngestStats, RawAircraft};
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
    let file = match File::open(path(dir, rel, spec)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // from_reader consumes the complete JSON; the multi-member decoder also checks
    // the gzip footer and rejects truncated/trailing data before a part is trusted.
    let cached: Checkpoint = serde_json::from_reader(BufReader::new(MultiGzDecoder::new(file)))
        .map_err(|e| DataError(format!("raw feed checkpoint: {e}")))?;
    if cached.identity != Identity::new(rel, spec, context) {
        return Ok(None);
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
                a_point.alt_geom_msl, a_point.alt_baro_m, a_point.gs_mps,
                a_point.track_deg, a_point.vrate_mps,
            ].map(f32::to_bits),
            [
                b_point.alt_geom_msl, b_point.alt_baro_m, b_point.gs_mps,
                b_point.track_deg, b_point.vrate_mps,
            ].map(f32::to_bits)
        );
        assert_eq!(
            (a_point.flags, a_point.new_leg, a_point.retained_break, a_point.retained_end),
            (b_point.flags, b_point.new_leg, b_point.retained_break, b_point.retained_end)
        );
        let ground_bits = |events: &BoundaryEvents| events.ground.iter()
            .map(|(lo, hi)| (lo.to_bits(), hi.to_bits())).collect::<Vec<_>>();
        let leg_bits = |events: &BoundaryEvents| events.new_leg.iter()
            .map(|t| t.to_bits()).collect::<Vec<_>>();
        assert_eq!(ground_bits(&a.leg_boundaries), ground_bits(&b.leg_boundaries));
        assert_eq!(leg_bits(&a.leg_boundaries), leg_bits(&b.leg_boundaries));

        // Missing data and a completely ingested empty feed are distinct states.
        let empty = (Vec::new(), IngestStats { bytes: 1024, ..Default::default() });
        save(&dir, &rel, &spec, context, &empty).unwrap();
        let restored = load(&dir, &rel, &spec, context).unwrap().unwrap();
        assert!(restored.0.is_empty());
        assert_eq!(restored.1, empty.1);
    }

    #[test]
    fn checkpoint_identity_requires_exact_release_filter_policy_and_context() {
        let (dir, rel, spec, context) = fixture();
        save(&dir, &rel, &spec, context, &part()).unwrap();
        let original_path = path(&dir, &rel, &spec);
        let changed_specs: [fn(&mut IngestSpec); 9] = [
            |s| s.bbox.south = f64::from_bits(s.bbox.south.to_bits() + 1),
            |s| s.bbox.west = f64::from_bits(s.bbox.west.to_bits() + 1),
            |s| s.bbox.north = f64::from_bits(s.bbox.north.to_bits() + 1),
            |s| s.bbox.east = f64::from_bits(s.bbox.east.to_bits() + 1),
            |s| s.t_start = f64::from_bits(s.t_start.to_bits() + 1),
            |s| s.t_end = f64::from_bits(s.t_end.to_bits() + 1),
            |s| s.geoid_n_m = f64::from_bits(s.geoid_n_m.to_bits() + 1),
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
            |r| { r.parts.pop(); },
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
            DayContext::new(spec.t_start, f64::from_bits(300.0_f64.to_bits() + 1), 3600.0),
            DayContext::new(spec.t_start, 300.0, f64::from_bits(3600.0_f64.to_bits() + 1)),
        ] {
            assert!(load(&dir, &rel, &spec, changed).unwrap().is_none());
        }
        assert!(load(&dir, &rel, &spec, context).unwrap().is_some());
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
            serde_json::to_writer(&mut encoder, &CheckpointRef {
                identity: &identity, aircraft: &part.0, stats: &part.1,
            }).unwrap();
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
