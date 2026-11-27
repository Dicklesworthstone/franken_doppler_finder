//! Offline sources for the ordinary ADS-B day cache. Canonical release filenames
//! identify a family; the embedded README confirms its day and feed during ingest.

use crate::DataError;
use crate::adsblol::{AssetPart, DayRelease, Feed, repo_for_year, tag_for, valid_archive_parts};
use crate::ingest::{
    DayContext, IngestProgress, IngestSpec, IngestStats, RawAircraft, ingest_local_day_reader,
};
use fdop_types::time::parse_date;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

const README_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Debug)]
struct LocalPart {
    path: PathBuf,
    size: u64,
}

#[derive(Debug)]
pub(crate) struct LocalRelease {
    pub day: (i64, u32, u32),
    pub release: DayRelease,
    parts: Vec<LocalPart>,
}

impl LocalRelease {
    pub fn ingest(
        &self,
        spec: &IngestSpec,
        context: DayContext,
        progress: &(dyn Fn(&IngestProgress) + Sync),
    ) -> Result<(Vec<RawAircraft>, IngestStats), DataError> {
        let result = ingest_local_day_reader(
            BufReader::with_capacity(1024 * 1024, LocalReader::new(self.parts.clone())),
            self.release.feed.label(),
            spec,
            progress,
            context,
            ArchiveProvenance::new(self.day, &self.release),
        )?;
        if result.1.members == 0 {
            return Err(DataError(format!(
                "local archive {} contains no trace members",
                self.release.tag
            )));
        }
        Ok(result)
    }
}

#[derive(Debug)]
struct ArchiveName {
    day: (i64, u32, u32),
    feed: Feed,
    tag: String,
}

fn archive_name(name: &str) -> Result<ArchiveName, DataError> {
    let invalid = || DataError(format!("not a canonical adsb.lol archive filename: {name}"));
    let tag = if let Some(tag) = name.strip_suffix(".tar") {
        tag
    } else {
        let (tag, suffix) = name.rsplit_once(".tar.").ok_or_else(invalid)?;
        if suffix.len() != 2 || !suffix.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(invalid());
        }
        tag
    };
    let (date, pod) = tag
        .strip_prefix('v')
        .and_then(|s| s.split_once("-planes-readsb-"))
        .ok_or_else(invalid)?;
    let day = parse_date(&date.replace('.', "-")).map_err(|_| invalid())?;
    let (word, revision) = pod.rsplit_once('-').ok_or_else(invalid)?;
    let feed = match word {
        "prod" => Feed::Prod,
        "mlatonly" => Feed::Mlatonly,
        "staging" => Feed::Staging,
        _ => return Err(invalid()),
    };
    let revision: u32 = revision.parse().map_err(|_| invalid())?;
    if revision > 3 || tag != tag_for(day.0, day.1, day.2, feed, revision) {
        return Err(invalid());
    }
    Ok(ArchiveName {
        day,
        feed,
        tag: tag.to_string(),
    })
}

fn archive_candidate(name: &str) -> bool {
    name.starts_with('v')
        && name.contains("-planes-readsb-")
        && (name.ends_with(".tar")
            || name.rsplit_once(".tar.").is_some_and(|(_, suffix)| {
                suffix.len() == 2 && suffix.bytes().all(|b| b.is_ascii_lowercase())
            }))
}

/// A file selects its complete sibling family; a directory selects all canonical
/// families immediately inside it. Repeated paths are harmless, but different
/// copies or revisions of a day's same feed require an unambiguous selection.
pub(crate) fn local_releases(paths: &[PathBuf]) -> Result<Vec<LocalRelease>, DataError> {
    let mut families: BTreeMap<String, (ArchiveName, BTreeMap<String, PathBuf>)> = BTreeMap::new();
    for input in paths {
        let metadata = fs::metadata(input)
            .map_err(|e| DataError(format!("local archive {}: {e}", input.display())))?;
        let (directory, selected) = if metadata.is_dir() {
            (input.as_path(), None)
        } else if metadata.is_file() {
            let name = input
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| DataError(format!("invalid archive path: {}", input.display())))?;
            (
                input.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")),
                Some(archive_name(name)?.tag),
            )
        } else {
            return Err(DataError(format!(
                "local archive is not a regular file or directory: {}",
                input.display()
            )));
        };
        let mut found = false;
        for entry in fs::read_dir(directory)
            .map_err(|e| DataError(format!("archive directory {}: {e}", directory.display())))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| archive_candidate(name)) else {
                continue;
            };
            if selected.as_ref().is_some_and(|tag| {
                name != format!("{tag}.tar") && !name.starts_with(&format!("{tag}.tar."))
            }) {
                continue;
            }
            let parsed = archive_name(name)?;
            let path = fs::canonicalize(entry.path())?;
            if !fs::metadata(&path)?.is_file() {
                return Err(DataError(format!("archive part is not a file: {}", path.display())));
            }
            let (_, parts) = families.entry(parsed.tag.clone()).or_insert_with(|| (parsed, BTreeMap::new()));
            if let Some(previous) = parts.insert(name.to_string(), path.clone())
                && previous != path
            {
                return Err(DataError(format!("multiple local copies of archive part {name}")));
            }
            found = true;
        }
        if !found {
            return Err(DataError(format!("no adsb.lol archives found in {}", input.display())));
        }
    }

    let mut feeds = BTreeSet::new();
    let mut out = Vec::with_capacity(families.len());
    for (_, (parsed, files)) in families {
        if !feeds.insert((parsed.day, parsed.feed.tag_word())) {
            return Err(DataError(format!("multiple local release revisions for {}", parsed.tag)));
        }
        let repo = repo_for_year(parsed.day.0);
        let mut parts = Vec::with_capacity(files.len());
        let mut assets = Vec::with_capacity(files.len());
        for (name, path) in files {
            let size = fs::metadata(&path)?.len();
            assets.push(AssetPart {
                url: format!("https://github.com/{repo}/releases/download/{}/{name}", parsed.tag),
                name,
                size,
            });
            parts.push(LocalPart { path, size });
        }
        if !valid_archive_parts(&parsed.tag, &assets) {
            return Err(DataError(format!(
                "incomplete local archive family {}: use one .tar or nonempty consecutive .tar.aa, .tar.ab, ... parts",
                parsed.tag
            )));
        }
        let first_size = parts[0].size;
        if parts[..parts.len() - 1].iter().any(|p| p.size != first_size)
            || parts.last().is_some_and(|p| p.size > first_size)
        {
            return Err(DataError(format!(
                "invalid split sizes for {}: nonfinal parts must be equal and the final part no larger",
                parsed.tag
            )));
        }
        out.push(LocalRelease {
            day: parsed.day,
            release: DayRelease { feed: parsed.feed, repo, tag: parsed.tag, parts: assets },
            parts,
        });
    }
    Ok(out)
}

/// A bounded concatenation. Every part is consumed to its own EOF, including
/// padding after tar's first end marker; length changes never become clean EOF.
struct LocalReader {
    remaining: std::vec::IntoIter<LocalPart>,
    current: Option<(File, LocalPart, u64)>,
}

impl LocalReader {
    fn new(parts: Vec<LocalPart>) -> Self {
        Self { remaining: parts.into_iter(), current: None }
    }
}

impl Read for LocalReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() { return Ok(0); }
        loop {
            if let Some((file, part, left)) = &mut self.current {
                if *left > 0 {
                    let take = (*left).min(buf.len() as u64) as usize;
                    let n = file.read(&mut buf[..take])?;
                    if n == 0 {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof,
                            format!("local archive part truncated: {}", part.path.display())));
                    }
                    *left -= n as u64;
                    return Ok(n);
                }
                if file.read(&mut [0u8; 1])? != 0 || file.metadata()?.len() != part.size {
                    return Err(io::Error::new(io::ErrorKind::InvalidData,
                        format!("local archive part size changed: {}", part.path.display())));
                }
                self.current = None;
            }
            let Some(part) = self.remaining.next() else { return Ok(0); };
            let file = File::open(&part.path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() != part.size {
                return Err(io::Error::new(io::ErrorKind::InvalidData,
                    format!("local archive part size changed: {}", part.path.display())));
            }
            let left = part.size;
            self.current = Some((file, part, left));
        }
    }
}

/// Validated in the existing tar producer, before any local checkpoint is saved.
pub(crate) struct ArchiveProvenance {
    day: (i64, u32, u32),
    tag: String,
    seen: bool,
}

impl ArchiveProvenance {
    fn new(day: (i64, u32, u32), release: &DayRelease) -> Self {
        Self { day, tag: release.tag.clone(), seen: false }
    }

    pub(crate) fn check_member(
        &mut self,
        name: &str,
        size: u64,
        reader: &mut impl Read,
    ) -> Result<(), DataError> {
        if !matches!(name, "README.txt" | "./README.txt") {
            return Ok(());
        }
        if self.seen {
            return Err(DataError(format!("duplicate archive README.txt for {}", self.tag)));
        }
        self.seen = true;
        if size > README_LIMIT {
            return Err(DataError("archive README.txt exceeds 64 KiB".into()));
        }
        let mut body = String::new();
        reader.take(README_LIMIT + 1).read_to_string(&mut body)
            .map_err(|e| DataError(format!("archive README.txt: {e}")))?;
        if body.len() as u64 != size {
            return Err(DataError("archive README.txt length mismatch".into()));
        }
        let (y, m, d) = self.day;
        let pod = self.tag.split_once("-planes-readsb-").map(|(_, pod)| format!("planes-readsb-{pod}")).unwrap_or_default();
        for (prefix, expected) in [
            ("GitHub Release: ", self.tag.clone()),
            ("Original Path: ", format!("/var/globe_history/{y:04}/{m:02}/{d:02}")),
            ("Pod of origin: ", pod),
            ("This is a database for the day of ", format!("{y:04}-{m:02}-{d:02} of all aircraft known to adsb.lol.")),
        ] {
            let mut fields = body.lines().filter_map(|line| line.strip_prefix(prefix));
            if fields.next() != Some(expected.as_str()) || fields.next().is_some() {
                return Err(DataError(format!(
                    "archive README.txt does not uniquely confirm {prefix}{expected}"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(), DataError> {
        if self.seen { Ok(()) } else {
            Err(DataError(format!("local archive {} is missing its top-level README.txt provenance", self.tag)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{T0, gz, pc12_json, spec, tar_of};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const TAG: &str = "v2026.09.08-planes-readsb-prod-0";

    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fdop-local-archive-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn readme() -> String {
        format!(
            "This is a database for the day of 2026-09-08 of all aircraft known to adsb.lol.\nGitHub Release: {TAG}\nOriginal Path: /var/globe_history/2026/09/08\nPod of origin: planes-readsb-prod-0\n"
        )
    }

    fn archive() -> Vec<u8> {
        tar_of(&[
            ("./README.txt", readme().into_bytes()),
            ("./traces/c3/trace_full_a1b2c3.json", gz(&pc12_json())),
        ])
    }

    fn write_parts(dir: &Path, bytes: &[u8], chunk: usize) -> Vec<PathBuf> {
        bytes.chunks(chunk).enumerate().map(|(i, b)| {
            let path = dir.join(format!("{TAG}.tar.{}{}", char::from(b'a' + (i / 26) as u8), char::from(b'a' + (i % 26) as u8)));
            fs::write(&path, b).unwrap();
            path
        }).collect()
    }

    fn context() -> DayContext { DayContext::new(T0, 300.0, 3600.0) }

    #[test]
    fn full_and_split_archives_preserve_the_normal_day_ingest() {
        let bytes = archive();
        let full = directory().join(format!("{TAG}.tar"));
        fs::write(&full, &bytes).unwrap();
        let split_dir = directory();
        // Cross tar headers, compressed trace data and both end-marker blocks.
        let paths = write_parts(&split_dir, &bytes, 713);
        let full = local_releases(&[full]).unwrap().remove(0);
        let split = local_releases(&[paths.last().unwrap().clone(), paths[0].clone(), split_dir]).unwrap().remove(0);
        assert_eq!(split.release.parts.len(), paths.len());
        assert_eq!(split.day, (2026, 9, 8));
        assert_eq!(split.release.repo, "adsblol/globe_history_2026");
        assert_eq!(split.release.parts[0].url, format!("https://github.com/adsblol/globe_history_2026/releases/download/{TAG}/{TAG}.tar.aa"));
        let expected = crate::ingest::ingest_day_reader(bytes.as_slice(), "prod", &spec(), &|_| {}, context()).unwrap();
        assert_eq!(expected.0.len(), 1);
        for source in [full, split] {
            let actual = source.ingest(&spec(), context(), &|_| {}).unwrap();
            assert_eq!(serde_json::to_value(&actual.0).unwrap(), serde_json::to_value(&expected.0).unwrap());
            assert_eq!(actual.1.bytes, bytes.len() as u64);
            assert_eq!(actual.1.members, 1);
            assert_eq!(actual.1.points_kept, expected.1.points_kept);
            let elsewhere = IngestSpec { bbox: fdop_types::BBox::new(0.0, 0.0, 1.0, 1.0), ..spec() };
            let empty = source.ingest(&elsewhere, context(), &|_| {}).unwrap();
            assert!(empty.0.is_empty(), "valid regional emptiness must remain cacheable");
            assert_eq!(empty.1.members, 1);
        }
    }

    #[test]
    fn canonical_names_preserve_day_feed_and_revision() {
        for feed in [Feed::Prod, Feed::Mlatonly, Feed::Staging] {
            for revision in 0..4 {
                let tag = tag_for(2024, 2, 29, feed, revision);
                let parsed = archive_name(&format!("{tag}.tar.az")).unwrap();
                assert_eq!(parsed.day, (2024, 2, 29));
                assert_eq!(parsed.feed, feed);
                assert_eq!(parsed.tag, tag);
            }
        }
        for bad in [
            "v2026.02.29-planes-readsb-prod-0.tar",
            "v2026.9.08-planes-readsb-prod-0.tar",
            "v2026.09.08-planes-readsb-mlat-0.tar",
            "v2026.09.08-planes-readsb-prod-4.tar",
            "v2026.09.08-planes-readsb-prod-00.tar",
            "v2026.09.08-planes-readsb-prod-0.tar.a",
            "v2026.09.08-planes-readsb-prod-0.tar.AA",
            "traces.tar",
        ] {
            assert!(archive_name(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn incomplete_mixed_and_inconsistent_families_are_rejected() {
        for entries in [
            vec![("ab", 16)],
            vec![("aa", 16), ("ac", 16)],
            vec![("", 16), ("aa", 16)],
            vec![("aa", 16), ("ab", 0)],
            vec![("aa", 16), ("ab", 17)],
            vec![("aa", 16), ("ab", 8), ("ac", 8)],
        ] {
            let dir = directory();
            for (suffix, size) in entries {
                let suffix = if suffix.is_empty() { String::new() } else { format!(".{suffix}") };
                fs::write(dir.join(format!("{TAG}.tar{suffix}")), vec![0; size]).unwrap();
            }
            assert!(local_releases(&[dir]).is_err());
        }
        let a = directory();
        let b = directory();
        fs::write(a.join(format!("{TAG}.tar")), archive()).unwrap();
        fs::write(b.join(format!("{TAG}.tar")), archive()).unwrap();
        assert!(local_releases(&[a.clone(), b]).unwrap_err().0.contains("multiple local copies"));
        fs::write(a.join("v2026.09.08-planes-readsb-prod-1.tar"), archive()).unwrap();
        assert!(local_releases(&[a]).unwrap_err().0.contains("multiple local release revisions"));
    }

    #[test]
    fn final_part_requires_all_members_and_both_tar_end_blocks() {
        let bytes = archive();
        let payload_end = bytes.len() - 1024;
        let trace_len = gz(&pc12_json()).len();
        assert!(trace_len < 512);
        let partial_trace = payload_end - 512 + trace_len / 2;
        for end in [partial_trace, payload_end - 1, payload_end, payload_end + 512, bytes.len() - 1] {
            let dir = directory();
            // A missing .ab can still leave complete member padding or one zero
            // block in .aa; neither establishes a complete archive.
            fs::write(dir.join(format!("{TAG}.tar.aa")), &bytes[..end]).unwrap();
            let local = local_releases(&[dir]).unwrap().remove(0);
            assert!(local.ingest(&spec(), context(), &|_| {}).is_err(), "accepted {end} of {} bytes", bytes.len());
        }
        let dir = directory();
        fs::write(dir.join(format!("{TAG}.tar.aa")), &bytes).unwrap();
        fs::write(dir.join(format!("{TAG}.tar.ab")), b"unconsumed later part").unwrap();
        let local = local_releases(&[dir]).unwrap().remove(0);
        assert!(local.ingest(&spec(), context(), &|_| {}).unwrap_err().0.contains("nonzero data after its end marker"));
    }

    #[test]
    fn changed_part_sizes_are_not_clean_eof() {
        let dir = directory();
        let path = dir.join(format!("{TAG}.tar"));
        fs::write(&path, archive()).unwrap();
        let local = local_releases(std::slice::from_ref(&path)).unwrap().remove(0);
        File::options().append(true).open(&path).unwrap().write_all(b"growth").unwrap();
        assert!(local.ingest(&spec(), context(), &|_| {}).unwrap_err().0.contains("size changed"));

        fs::write(&path, [1u8; 16]).unwrap();
        let mut reader = LocalReader::new(vec![LocalPart { path: path.clone(), size: 16 }]);
        reader.read_exact(&mut [0u8; 4]).unwrap();
        File::options().write(true).open(path).unwrap().set_len(8).unwrap();
        assert_eq!(reader.read_to_end(&mut Vec::new()).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn filename_cannot_substitute_for_embedded_day_and_feed_provenance() {
        let valid = readme().into_bytes();
        let cases = vec![
            vec![],
            vec![("nested/README.txt", valid.clone())],
            vec![("README.txt", readme().replace("2026-09-08", "2026-09-09").into_bytes())],
            vec![("README.txt", readme().replace("planes-readsb-prod-0", "planes-readsb-mlatonly-0").into_bytes())],
            vec![("README.txt", format!("{}GitHub Release: {TAG}\n", readme()).into_bytes())],
            vec![("README.txt", valid.clone()), ("./README.txt", valid)],
            vec![("README.txt", vec![b' '; README_LIMIT as usize + 1])],
        ];
        for mut members in cases {
            members.push(("trace_full_a1b2c3.json", gz(&pc12_json())));
            let path = directory().join(format!("{TAG}.tar"));
            fs::write(&path, tar_of(&members)).unwrap();
            let local = local_releases(&[path]).unwrap().remove(0);
            assert!(local.ingest(&spec(), context(), &|_| {}).is_err());
        }
        let path = directory().join(format!("{TAG}.tar"));
        fs::write(&path, tar_of(&[("README.txt", readme().into_bytes())])).unwrap();
        let local = local_releases(&[path]).unwrap().remove(0);
        assert!(local.ingest(&spec(), context(), &|_| {}).unwrap_err().0.contains("no trace members"));
    }
}
