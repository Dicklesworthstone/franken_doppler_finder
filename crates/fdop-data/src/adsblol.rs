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

/// [`find_day_releases`] with a JSON cache at `<dir>/releases.json` (only non-empty results are
/// cached; a missing day is re-checked next time). `offline` never touches the network.
pub fn find_day_releases_cached(
    net: Option<&Net>,
    y: i64,
    m: u32,
    d: u32,
    dir: &Path,
    offline: bool,
) -> Result<Vec<DayRelease>, DataError> {
    let path = dir.join("releases.json");
    if let Ok(b) = std::fs::read(&path)
        && let Ok(v) = serde_json::from_slice::<Vec<DayRelease>>(&b)
        && !v.is_empty()
    {
        return Ok(v);
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
