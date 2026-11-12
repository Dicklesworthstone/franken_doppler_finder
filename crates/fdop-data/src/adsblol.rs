//! adsb.lol `globe_history_{year}` release discovery.
//!
//! One release per UTC day and feed, tagged `v{YYYY.MM.DD}-planes-readsb-{feed}-{N}`
//! (verified for 2023-2026; some days are missing). Large releases are split into
//! `.tar.aa`, `.tar.ab`, ... parts that concatenate into one tar stream.

use crate::DataError;
use fdop_http::Net;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Feed {
    /// Main readsb feed (ADS-B + MLAT + TIS-B as received by adsb.lol).
    Prod,
    /// MLAT-only positions (adds Mode-S aircraft without ADS-B out).
    Mlatonly,
    /// Parallel staging instance; used only when a day's prod release is missing.
    Staging,
}

impl Feed {
    /// Word used in release tags.
    pub fn tag_word(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Mlatonly => "mlatonly",
            Self::Staging => "staging",
        }
    }
    /// Short label stored in `FlightLeg::source`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Mlatonly => "mlat",
            Self::Staging => "staging",
        }
    }
    /// Preference when the same instant appears in several feeds (lower wins).
    pub fn rank(self) -> u8 {
        match self {
            Self::Prod => 0,
            Self::Staging => 1,
            Self::Mlatonly => 2,
        }
    }
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "prod" => Some(Self::Prod),
            "mlat" | "mlatonly" => Some(Self::Mlatonly),
            "staging" => Some(Self::Staging),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetPart {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayRelease {
    pub feed: Feed,
    pub repo: String,
    pub tag: String,
    /// Ordered parts; concatenated they form one tar stream.
    pub parts: Vec<AssetPart>,
}

impl DayRelease {
    pub fn total_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.size).sum()
    }
    pub fn urls(&self) -> Vec<String> {
        self.parts.iter().map(|p| p.url.clone()).collect()
    }
}

pub fn repo_for_year(y: i64) -> String {
    format!("adsblol/globe_history_{y}")
}

pub fn tag_for(y: i64, m: u32, d: u32, feed: Feed, n: u32) -> String {
    format!(
        "v{y:04}.{m:02}.{d:02}-planes-readsb-{}-{n}",
        feed.tag_word()
    )
}

fn api_headers() -> Vec<(String, String)> {
    let mut h = vec![
        (
            "Accept".to_string(),
            "application/vnd.github+json".to_string(),
        ),
        ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()),
    ];
    if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
        let tok = tok.trim().to_string();
        if !tok.is_empty() {
            h.push(("Authorization".to_string(), format!("Bearer {tok}")));
        }
    }
    h
}

/// A release is either one tar or the consecutive split pieces of that same tar.
/// A final missing piece is detected by the tar terminator when the stream is ingested.
fn valid_archive_parts(tag: &str, parts: &[AssetPart]) -> bool {
    if parts.is_empty()
        || parts.len() > 26 * 26
        || parts.iter().any(|p| p.size == 0 || p.url.is_empty())
    {
        return false;
    }
    let stem = format!("{tag}.tar");
    if parts.len() == 1 && parts[0].name == stem {
        return true;
    }
    parts.iter().enumerate().all(|(i, p)| {
        p.name
            == format!(
                "{stem}.{}{}",
                char::from(b'a' + (i / 26) as u8),
                char::from(b'a' + (i % 26) as u8)
            )
    })
}

/// Parse a usable uploaded tar asset set, ignoring sidecars and unrelated files.
/// Split parts must start at .aa with no gaps or duplicates; an incomplete upload
/// or a mixture of the full tar and split pieces is not a usable release.
pub fn parse_release_json(v: &serde_json::Value, feed: Feed, repo: &str) -> Option<DayRelease> {
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let stem = format!("{tag}.tar");
    let prefix = format!("{stem}.");
    let mut parts = Vec::new();
    for asset in v.get("assets")?.as_array()? {
        let Some(name) = asset.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let split = name.strip_prefix(&prefix).is_some_and(|suffix| {
            suffix.len() == 2 && suffix.bytes().all(|b| b.is_ascii_lowercase())
        });
        if name != stem && !split {
            continue;
        }
        if asset.get("state").is_some_and(|s| s.as_str() != Some("uploaded")) {
            return None;
        }
        parts.push(AssetPart {
            name: name.to_string(),
            url: asset.get("browser_download_url")?.as_str()?.to_string(),
            size: asset.get("size")?.as_u64()?,
        });
    }
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    valid_archive_parts(&tag, &parts).then_some(DayRelease {
        feed,
        repo: repo.to_string(),
        tag,
        parts,
    })
}

/// Look up one feed's release for a UTC day, trying suffixes `-0` .. `-3`.
pub fn find_release(
    net: &Net,
    y: i64,
    m: u32,
    d: u32,
    feed: Feed,
) -> Result<Option<DayRelease>, DataError> {
    let repo = repo_for_year(y);
    let hdrs = api_headers();
    let refs: Vec<(&str, &str)> = hdrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    for n in 0..4 {
        let tag = tag_for(y, m, d, feed, n);
        let url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");
        match net.get(&url, &refs) {
            Ok(r) => {
                let v = r.json().map_err(|e| DataError(format!("{url}: {e}")))?;
                if let Some(rel) = parse_release_json(&v, feed, &repo) {
                    return Ok(Some(rel));
                }
            }
            Err(e) if e.status == Some(404) => continue,
            Err(e) if e.status == Some(403) || e.status == Some(429) => {
                return Err(DataError(format!(
                    "GitHub API refused {url} ({e}); unauthenticated calls are limited to 60/hour, set GITHUB_TOKEN"
                )));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

/// A completed main lookup, optionally with an independently failed MLAT lookup.
/// An optional failure must never certify a complete release listing.
#[derive(Debug)]
pub(crate) struct ReleaseDiscovery {
    pub releases: Vec<DayRelease>,
    pub mlat_error: Option<DataError>,
}

impl ReleaseDiscovery {
    fn strict(self) -> Result<Vec<DayRelease>, DataError> {
        match self.mlat_error {
            Some(error) => Err(error),
            None => Ok(self.releases),
        }
    }
}

fn discover_day_releases(
    mut lookup: impl FnMut(Feed) -> Result<Option<DayRelease>, DataError>,
    include_mlat: bool,
) -> Result<ReleaseDiscovery, DataError> {
    let mut out = Vec::new();
    match lookup(Feed::Prod)? {
        Some(r) => out.push(r),
        None => {
            if let Some(r) = lookup(Feed::Staging)? {
                out.push(r);
            }
        }
    }
    let mut mlat_error = None;
    if include_mlat {
        match lookup(Feed::Mlatonly) {
            Ok(Some(r)) => out.push(r),
            Ok(None) => {}
            Err(error) if !out.is_empty() => mlat_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Ok(ReleaseDiscovery {
        releases: out,
        mlat_error,
    })
}

/// Prod and MLAT-only releases for a UTC day. Staging is looked up only when prod is missing.
/// This public full-discovery API remains strict when either requested lookup fails.
pub fn find_day_releases(net: &Net, y: i64, m: u32, d: u32) -> Result<Vec<DayRelease>, DataError> {
    discover_day_releases(|feed| find_release(net, y, m, d, feed), true)?.strict()
}

/// True when a listing has everything a day can have (prod and mlatonly).
pub fn releases_complete(v: &[DayRelease]) -> bool {
    v.iter().any(|r| r.feed == Feed::Prod) && v.iter().any(|r| r.feed == Feed::Mlatonly)
}

/// True when the UTC day ended more than [`crate::fetch::PUBLISH_LAG_S`] ago: whatever releases
/// it has now are all it will ever have.
pub fn day_settled(y: i64, m: u32, d: u32) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_secs_f64())
        .unwrap_or(f64::INFINITY);
    day_settled_at(y, m, d, now)
}

fn day_settled_at(y: i64, m: u32, d: u32, checked_at: f64) -> bool {
    let end = (fdop_types::time::days_from_civil(y, m, d) + 1) as f64 * 86_400.0;
    end + crate::fetch::PUBLISH_LAG_S < checked_at
}

/// Only whole-tar listings fetched after the publication window can be final.
/// Split releases have no asset-count manifest: even a contiguous prefix can omit the
/// last piece. Re-list them before an uncached download; complete day-store hits bypass
/// release lookup altogether. Missing lookup-time evidence also requires an online check.
fn cached_listing_final(v: &[DayRelease], y: i64, m: u32, d: u32, path: &Path) -> bool {
    !v.is_empty()
        && v.iter().all(|r| valid_archive_parts(&r.tag, &r.parts))
        && v.iter().all(|r| r.parts.len() == 1 && r.parts[0].name.ends_with(".tar"))
        && std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|t| day_settled_at(y, m, d, t.as_secs_f64()))
}

/// [`find_day_releases`] with a JSON cache at `<dir>/releases.json`. Only non-empty results are
/// cached. Every listing fetched before the day [settled](day_settled), and every split-archive
/// listing regardless of age, is re-checked: missing feeds or final split assets may have been
/// published since that lookup. `offline` never touches the network and uses valid cached sets.
pub fn find_day_releases_cached(
    net: Option<&Net>,
    y: i64,
    m: u32,
    d: u32,
    dir: &Path,
    offline: bool,
) -> Result<Vec<DayRelease>, DataError> {
    let path = dir.join("releases.json");
    let cached = read_cached_listing(&path);
    if let Some(v) = &cached
        && (offline || cached_listing_final(v, y, m, d, &path))
    {
        return Ok(v.clone());
    }
    if offline {
        return Ok(Vec::new());
    }
    let owned;
    let net = match net {
        Some(n) => n,
        None => {
            owned = Net::new()?;
            &owned
        }
    };
    let rels = find_day_releases(net, y, m, d)?;
    if !rels.is_empty() {
        std::fs::create_dir_all(dir)?;
        fdop_http::write_atomic(
            &path,
            &serde_json::to_vec_pretty(&rels).map_err(|e| DataError(e.to_string()))?,
        )?;
    }
    Ok(rels)
}

fn read_cached_listing(path: &Path) -> Option<Vec<DayRelease>> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<DayRelease>>(&b).ok())
        .filter(|v| !v.is_empty() && v.iter().all(|r| valid_archive_parts(&r.tag, &r.parts)))
}

/// Fetch/availability discovery can use a completed main lookup without MLAT. Its separate
/// cache records only main-feed evidence; an online full request always retries MLAT unless
/// the original full listing is reusable under its existing finality rules.
pub(crate) fn find_day_releases_for_fetch_cached(
    net: Option<&Net>,
    y: i64,
    m: u32,
    d: u32,
    dir: &Path,
    offline: bool,
    include_mlat: bool,
) -> Result<ReleaseDiscovery, DataError> {
    let mut owned = None;
    cached_fetch_releases(y, m, d, dir, offline, include_mlat, |feed| {
        let net = match net {
            Some(net) => net,
            None => {
                if owned.is_none() {
                    owned = Some(Net::new()?);
                }
                owned.as_ref().unwrap()
            }
        };
        find_release(net, y, m, d, feed)
    })
}

fn cached_fetch_releases(
    y: i64,
    m: u32,
    d: u32,
    dir: &Path,
    offline: bool,
    include_mlat: bool,
    lookup: impl FnMut(Feed) -> Result<Option<DayRelease>, DataError>,
) -> Result<ReleaseDiscovery, DataError> {
    let full_path = dir.join("releases.json");
    let main_path = dir.join("releases-main.json");
    if let Some(mut releases) = read_cached_listing(&full_path)
        && (offline || cached_listing_final(&releases, y, m, d, &full_path))
    {
        if !include_mlat {
            releases.retain(|r| r.feed != Feed::Mlatonly);
        }
        // Offline, a completed main lookup can supplement a listing containing only MLAT.
        // Existing main entries keep their priority, and this combined view is never certified.
        if offline
            && !releases.iter().any(|r| r.feed != Feed::Mlatonly)
            && let Some(main) = read_cached_listing(&main_path)
            && main.iter().all(|r| r.feed != Feed::Mlatonly)
        {
            releases.splice(0..0, main);
        }
        if !releases.is_empty() || !offline {
            return Ok(ReleaseDiscovery {
                releases,
                mlat_error: None,
            });
        }
    }
    // A main-only result cannot prove that a requested optional feed is absent online.
    if (!include_mlat || offline)
        && let Some(releases) = read_cached_listing(&main_path)
        && releases.iter().all(|r| r.feed != Feed::Mlatonly)
        && (offline || cached_listing_final(&releases, y, m, d, &main_path))
    {
        return Ok(ReleaseDiscovery {
            releases,
            mlat_error: None,
        });
    }
    if offline {
        return Ok(ReleaseDiscovery {
            releases: Vec::new(),
            mlat_error: None,
        });
    }
    let found = discover_day_releases(lookup, include_mlat)?;
    if !found.releases.is_empty() {
        let path = if include_mlat && found.mlat_error.is_none() {
            &full_path
        } else {
            &main_path
        };
        std::fs::create_dir_all(dir)?;
        fdop_http::write_atomic(
            path,
            &serde_json::to_vec_pretty(&found.releases).map_err(|e| DataError(e.to_string()))?,
        )?;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached_release(feed: Feed) -> DayRelease {
        DayRelease {
            feed,
            repo: repo_for_year(2026),
            tag: tag_for(2026, 9, 8, feed, 0),
            parts: vec![AssetPart {
                name: format!("{}.tar", tag_for(2026, 9, 8, feed, 0)),
                url: format!("https://example.invalid/{}.tar", feed.tag_word()),
                size: 1024,
            }],
        }
    }

    fn cache_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fdop-release-cache-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_listing(path: &Path, releases: &[DayRelease], checked_at: f64) {
        std::fs::write(path, serde_json::to_vec(releases).unwrap()).unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(checked_at))
            .unwrap();
    }

    #[test]
    fn main_only_discovery_uses_its_own_cache_and_never_queries_mlat() {
        for main in [Feed::Prod, Feed::Staging] {
            let dir = cache_dir(main.tag_word());
            let release = cached_release(main);
            let mut calls = Vec::new();
            let found = cached_fetch_releases(2026, 9, 8, &dir, false, false, |feed| {
                calls.push(feed);
                assert_ne!(feed, Feed::Mlatonly);
                Ok((feed == main).then(|| release.clone()))
            })
            .unwrap();
            assert_eq!(found.releases, [release.clone()]);
            assert!(found.mlat_error.is_none());
            assert_eq!(
                calls,
                if main == Feed::Prod {
                    vec![Feed::Prod]
                } else {
                    vec![Feed::Prod, Feed::Staging]
                }
            );
            assert!(!dir.join("releases.json").exists());
            let main_path = dir.join("releases-main.json");
            assert_eq!(read_cached_listing(&main_path).unwrap(), [release.clone()]);
            let settled = fdop_types::time::unix_from_utc(2026, 9, 11, 1, 0, 0.0);
            write_listing(&main_path, std::slice::from_ref(&release), settled);
            for offline in [false, true] {
                let cached = cached_fetch_releases(2026, 9, 8, &dir, offline, false, |_| {
                    panic!("a usable main listing must avoid all network lookups")
                })
                .unwrap();
                assert_eq!(cached.releases, [release.clone()]);
            }
            // A completed full listing can serve this narrower request without being rewritten.
            let path = dir.join("releases.json");
            write_listing(
                &path,
                &[release.clone(), cached_release(Feed::Mlatonly)],
                settled,
            );
            let bytes = std::fs::read(&path).unwrap();
            let cached = cached_fetch_releases(2026, 9, 8, &dir, false, false, |_| {
                panic!("the established full listing has priority")
            })
            .unwrap();
            assert_eq!(cached.releases, [release]);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn optional_lookup_outage_keeps_main_without_certifying_a_full_listing() {
        for main in [Feed::Prod, Feed::Staging] {
            for existing_full in [false, true] {
                let dir = cache_dir("optional-outage");
                let path = dir.join("releases.json");
                let release = cached_release(main);
                if existing_full {
                    let provisional = fdop_types::time::unix_from_utc(2026, 9, 9, 1, 0, 0.0);
                    write_listing(&path, std::slice::from_ref(&release), provisional);
                }
                let before = std::fs::read(&path).ok();
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let found = cached_fetch_releases(2026, 9, 8, &dir, false, true, |feed| {
                    if feed == Feed::Mlatonly {
                        Err(DataError("optional API unavailable".into()))
                    } else {
                        Ok((feed == main).then(|| release.clone()))
                    }
                })
                .unwrap();
                assert_eq!(found.releases, [release.clone()]);
                assert!(
                    found
                        .mlat_error
                        .as_ref()
                        .unwrap()
                        .to_string()
                        .contains("optional API")
                );
                assert_eq!(std::fs::read(&path).ok(), before);
                assert_eq!(
                    std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
                    modified,
                    "an incomplete lookup must not advance the full listing's certification"
                );
                assert_eq!(
                    read_cached_listing(&dir.join("releases-main.json")).unwrap(),
                    [release.clone()]
                );
                assert!(
                    found
                        .strict()
                        .unwrap_err()
                        .to_string()
                        .contains("optional API")
                );
                // Even a settled main cache must not suppress a later requested MLAT lookup.
                let mut queried_mlat = false;
                let complete = cached_fetch_releases(2026, 9, 8, &dir, false, true, |feed| {
                    if feed == Feed::Mlatonly {
                        queried_mlat = true;
                        Ok(Some(cached_release(feed)))
                    } else {
                        Ok((feed == main).then(|| release.clone()))
                    }
                })
                .unwrap();
                assert!(queried_mlat);
                assert!(complete.mlat_error.is_none());
                assert_eq!(complete.releases, [release, cached_release(Feed::Mlatonly)]);
                assert_eq!(
                    find_day_releases_cached(None, 2026, 9, 8, &dir, true).unwrap(),
                    complete.releases
                );
            }
        }
    }

    #[test]
    fn main_lookup_errors_remain_fatal() {
        let dir = cache_dir("main-error");
        let error = cached_fetch_releases(2026, 9, 8, &dir, false, true, |feed| {
            assert_eq!(feed, Feed::Prod);
            Err(DataError("main API unavailable".into()))
        })
        .unwrap_err();
        assert!(error.to_string().contains("main API"));
        assert!(!dir.join("releases.json").exists());
        assert!(!dir.join("releases-main.json").exists());
    }

    #[test]
    fn provisional_release_cache_does_not_become_final_with_age() {
        let dir = cache_dir("provisional");
        let path = dir.join("releases.json");
        let end = fdop_types::time::unix_from_utc(2026, 9, 9, 0, 0, 0.0);
        let settled = end + crate::fetch::PUBLISH_LAG_S;
        let releases = vec![cached_release(Feed::Prod)];

        // A lookup just after the day ended can precede MLAT publication. Even when
        // the day is long settled now, that listing still needs an online refresh.
        write_listing(&path, &releases, end + 3600.0);
        assert!(day_settled_at(2026, 9, 8, settled + 30.0 * 86_400.0));
        assert!(!cached_listing_final(&releases, 2026, 9, 8, &path));
        assert_eq!(
            find_day_releases_cached(None, 2026, 9, 8, &dir, true).unwrap(),
            releases,
            "offline runs must keep using the available partial listing"
        );

        // Once a successful lookup after the publication window still finds no MLAT,
        // the missing feed is genuinely settled and later runs need no new API lookup.
        write_listing(&path, &releases, settled + 1.0);
        assert!(cached_listing_final(&releases, 2026, 9, 8, &path));
        for checked_at in [settled - 1.0, settled] {
            write_listing(&path, &releases, checked_at);
            assert!(!cached_listing_final(&releases, 2026, 9, 8, &path));
        }
    }

    #[test]
    fn both_feed_release_cache_stays_provisional_while_assets_can_arrive() {
        let dir = cache_dir("complete");
        let path = dir.join("releases.json");
        let checked_at = fdop_types::time::unix_from_utc(2026, 9, 9, 1, 0, 0.0);
        let prod = cached_release(Feed::Prod);
        let mlat = cached_release(Feed::Mlatonly);
        let staging = cached_release(Feed::Staging);
        for releases in [vec![prod.clone(), mlat.clone()], vec![staging, mlat]] {
            write_listing(&path, &releases, checked_at);
            assert!(
                !cached_listing_final(&releases, 2026, 9, 8, &path),
                "both feed releases can exist before the final split asset is uploaded"
            );
            assert_eq!(
                find_day_releases_cached(None, 2026, 9, 8, &dir, true).unwrap(),
                releases
            );
            let settled = checked_at - 3600.0 + crate::fetch::PUBLISH_LAG_S + 1.0;
            write_listing(&path, &releases, settled);
            assert!(cached_listing_final(&releases, 2026, 9, 8, &path));
        }
        let absent = dir.join("missing.json");
        assert!(!cached_listing_final(
            std::slice::from_ref(&prod),
            2026,
            9,
            8,
            &absent
        ));
        assert!(!cached_listing_final(
            &[prod, cached_release(Feed::Mlatonly)],
            2026,
            9,
            8,
            &absent
        ));
    }

    #[test]
    fn tags_and_parts_order() {
        assert_eq!(
            tag_for(2026, 9, 8, Feed::Prod, 0),
            "v2026.09.08-planes-readsb-prod-0"
        );
        assert_eq!(
            tag_for(2025, 1, 2, Feed::Mlatonly, 1),
            "v2025.01.02-planes-readsb-mlatonly-1"
        );
        let v = serde_json::json!({
            "tag_name": "v2026.09.08-planes-readsb-prod-0",
            "assets": [
                {"name": "v2026.09.08-planes-readsb-prod-0.tar.ab", "size": 2, "browser_download_url": "https://x/ab"},
                {"name": "notes.txt", "size": 1, "browser_download_url": "https://x/n"},
                {"name": "v2026.09.08-planes-readsb-prod-0.tar.aa", "size": 3, "browser_download_url": "https://x/aa"}
            ]
        });
        let r = parse_release_json(&v, Feed::Prod, "adsblol/globe_history_2026").unwrap();
        assert_eq!(
            r.urls(),
            vec!["https://x/aa".to_string(), "https://x/ab".to_string()]
        );
        assert_eq!(r.total_bytes(), 5);
        assert!(
            parse_release_json(
                &serde_json::json!({"tag_name": "x", "assets": []}),
                Feed::Prod,
                "r"
            )
            .is_none()
        );
    }

    #[test]
    fn release_parts_ignore_sidecars_and_require_a_complete_ordered_prefix() {
        let release = |names: &[&str]| {
            serde_json::json!({
                "tag_name": "day",
                "assets": names.iter().map(|name| serde_json::json!({
                    "name": name,
                    "size": 1024,
                    "state": "uploaded",
                    "browser_download_url": format!("https://example.invalid/{name}")
                })).collect::<Vec<_>>()
            })
        };
        for (names, expected_len) in [
            (vec!["day.tar", "day.tar.sha256", "notes.txt"], 1),
            (vec!["day.tar.ab", "day.tar.sha256", "day.tar.aa"], 2),
        ] {
            let parsed = parse_release_json(&release(&names), Feed::Prod, "r").unwrap();
            assert!(parsed.parts.iter().all(|p| !p.name.ends_with("sha256")));
            assert_eq!(parsed.parts.len(), expected_len);
        }
        for names in [
            vec!["day.tar.ab"],
            vec!["day.tar.aa", "day.tar.ac"],
            vec!["day.tar", "day.tar.aa"],
            vec!["day.tar.aa", "day.tar.aa"],
            vec!["unrelated.tar"],
        ] {
            assert!(
                parse_release_json(&release(&names), Feed::Prod, "r").is_none(),
                "{names:?}"
            );
        }
        for (key, value) in [
            ("size", serde_json::json!(0)),
            ("state", serde_json::json!("starter")),
            ("browser_download_url", serde_json::json!(null)),
        ] {
            let mut incomplete = release(&["day.tar.aa", "day.tar.ab"]);
            incomplete["assets"][1][key] = value;
            assert!(
                parse_release_json(&incomplete, Feed::Prod, "r").is_none(),
                "{key}"
            );
        }
    }

    #[test]
    fn malformed_cached_parts_do_not_become_a_final_release_listing() {
        let dir = cache_dir("missing-part");
        let path = dir.join("releases.json");
        let mut prod = cached_release(Feed::Prod);
        prod.parts[0].name.push_str(".ab"); // the .aa prefix is missing
        let checked_at = fdop_types::time::unix_from_utc(2026, 9, 11, 1, 0, 0.0);
        let releases = vec![prod, cached_release(Feed::Mlatonly)];
        write_listing(&path, &releases, checked_at);
        assert!(!cached_listing_final(&releases, 2026, 9, 8, &path));
        assert!(
            find_day_releases_cached(None, 2026, 9, 8, &dir, true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn split_release_listings_remain_refreshable_after_the_publication_window() {
        let dir = cache_dir("late-split-part");
        let path = dir.join("releases.json");
        let checked_at = fdop_types::time::unix_from_utc(2026, 9, 11, 1, 0, 0.0);
        assert!(day_settled_at(2026, 9, 8, checked_at));
        let mut prod = cached_release(Feed::Prod);
        prod.parts = ["aa", "ab"]
            .iter()
            .map(|suffix| AssetPart {
                name: format!("{}.tar.{suffix}", prod.tag),
                url: format!("https://example.invalid/archive.tar.{suffix}"),
                size: 2_000_000_000,
            })
            .collect();
        for part_count in [1, 2] {
            let mut partial = prod.clone();
            partial.parts.truncate(part_count);
            let releases = vec![partial, cached_release(Feed::Mlatonly)];
            write_listing(&path, &releases, checked_at);
            assert!(releases_complete(&releases));
            assert!(
                !cached_listing_final(&releases, 2026, 9, 8, &path),
                "feed presence and a contiguous split prefix cannot prove the final part exists"
            );
            assert_eq!(
                find_day_releases_cached(None, 2026, 9, 8, &dir, true).unwrap(),
                releases,
                "offline access preserves the available listing"
            );
        }
    }
}
