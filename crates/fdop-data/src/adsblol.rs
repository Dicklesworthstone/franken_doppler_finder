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

/// Parse a GitHub release JSON object into a [`DayRelease`] (tar parts only, name-ordered).
pub fn parse_release_json(v: &serde_json::Value, feed: Feed, repo: &str) -> Option<DayRelease> {
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let mut parts: Vec<AssetPart> = v
        .get("assets")?
        .as_array()?
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?.to_string();
            let is_tar = name.ends_with(".tar") || name.contains(".tar.");
            if !is_tar {
                return None;
            }
            Some(AssetPart {
                url: a.get("browser_download_url")?.as_str()?.to_string(),
                size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                name,
            })
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    Some(DayRelease {
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

/// Prod and MLAT-only releases for a UTC day. Staging is looked up only when prod is missing.
pub fn find_day_releases(net: &Net, y: i64, m: u32, d: u32) -> Result<Vec<DayRelease>, DataError> {
    let mut out = Vec::new();
    match find_release(net, y, m, d, Feed::Prod)? {
        Some(r) => out.push(r),
        None => {
            if let Some(r) = find_release(net, y, m, d, Feed::Staging)? {
                out.push(r);
            }
        }
    }
    if let Some(r) = find_release(net, y, m, d, Feed::Mlatonly)? {
        out.push(r);
    }
    Ok(out)
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

/// An incomplete listing is final only if it was fetched after the publication window.
/// A prod-only listing fetched before MLAT appeared does not become complete by ageing.
/// The atomic cache write records the successful lookup time in the file's mtime; if that
/// evidence is unavailable, an online caller must check again.
fn cached_listing_final(v: &[DayRelease], y: i64, m: u32, d: u32, path: &Path) -> bool {
    releases_complete(v)
        || std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|t| day_settled_at(y, m, d, t.as_secs_f64()))
}

/// [`find_day_releases`] with a JSON cache at `<dir>/releases.json`. Only non-empty results are
/// cached. An incomplete listing (no prod or no mlatonly) fetched before the day
/// [settled](day_settled) is re-checked, even on later runs: the missing release may have been
/// published since that lookup. `offline` never touches the network.
pub fn find_day_releases_cached(
    net: Option<&Net>,
    y: i64,
    m: u32,
    d: u32,
    dir: &Path,
    offline: bool,
) -> Result<Vec<DayRelease>, DataError> {
    let path = dir.join("releases.json");
    let cached = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<DayRelease>>(&b).ok())
        .filter(|v| !v.is_empty());
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
    if rels.is_empty()
        && let Some(v) = cached
    {
        return Ok(v);
    }
    if !rels.is_empty() {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("releases.json.part");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&rels).map_err(|e| DataError(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(rels)
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
                name: format!("{}.tar", feed.tag_word()),
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
    fn complete_release_cache_is_final_before_the_day_settles() {
        let dir = cache_dir("complete");
        let path = dir.join("releases.json");
        let checked_at = fdop_types::time::unix_from_utc(2026, 9, 9, 1, 0, 0.0);
        let prod = cached_release(Feed::Prod);
        let mlat = cached_release(Feed::Mlatonly);
        let staging = cached_release(Feed::Staging);
        for releases in [vec![prod.clone(), mlat.clone()], vec![staging, mlat]] {
            write_listing(&path, &releases, checked_at);
            assert_eq!(
                cached_listing_final(&releases, 2026, 9, 8, &path),
                releases_complete(&releases),
                "staging plus MLAT still needs a later check for prod"
            );
            assert_eq!(
                find_day_releases_cached(None, 2026, 9, 8, &dir, true).unwrap(),
                releases
            );
        }
        let absent = dir.join("missing.json");
        assert!(!cached_listing_final(
            std::slice::from_ref(&prod),
            2026,
            9,
            8,
            &absent
        ));
        assert!(cached_listing_final(
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
}
