//! Free-text "very approximate location" -> search region (bbox, centre, radius, UTC offsets).
//!
//! Accepted forms (case-insensitive, leading "near"/"around"/"somewhere in"/"outside" ignored):
//! `US` / `USA` / `CONUS` / `lower 48`; a state name or abbreviation; state lists
//! (`NY, NJ, CT`, `NY+NJ`); `City, ST`, `City ST`, `City New York`; `Suffolk County, NY`;
//! a 5-digit ZIP; `lat,lon`, `lat lon`, `lat,lon,radius_km` (also `40.8N 73.5W`);
//! a bbox `S,W,N,E`. Offline, only the CONUS/state/coordinate/bbox forms work unless the
//! Census gazetteer is already cached.

use crate::GeoError;
use crate::gazetteer::{GazRecord, Gazetteer};
use crate::states::{self, StateInfo, find_state, normalize_name};
use fdop_types::BBox;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    Conus,
    States,
    Place,
    County,
    Zip,
    Point,
    BBox,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// Human label, e.g. "New York", "Oyster Bay, NY", "ZIP 11771".
    pub label: String,
    pub kind: RegionKind,
    pub bbox: BBox,
    /// (lat, lon)
    pub center: (f64, f64),
    /// Radius around `center` for point-like regions.
    pub radius_km: Option<f64>,
    /// (most negative, least negative) UTC offset in hours over the region, DST included.
    pub utc_offset_h: (f64, f64),
    /// USPS codes of the states the region touches.
    pub states: Vec<String>,
}

/// Parsing context: where the gazetteer cache lives and whether downloads are allowed.
pub struct RegionContext {
    pub cache_dir: PathBuf,
    pub allow_network: bool,
    gaz: Gazetteer,
}

impl RegionContext {
    pub fn new(cache_dir: impl Into<PathBuf>, allow_network: bool) -> Self {
        let cache_dir = cache_dir.into();
        let gaz = Gazetteer::new(cache_dir.clone(), allow_network);
        Self {
            cache_dir,
            allow_network,
            gaz,
        }
    }
    /// Use an existing gazetteer (shares its loaded tables; also for tests).
    pub fn from_gazetteer(gaz: Gazetteer, allow_network: bool) -> Self {
        Self {
            cache_dir: PathBuf::new(),
            allow_network,
            gaz,
        }
    }
    pub fn gazetteer(&self) -> &Gazetteer {
        &self.gaz
    }
}

const CONUS_NAMES: &[&str] = &[
    "us",
    "usa",
    "u s",
    "us of a",
    "america",
    "united states",
    "united states of america",
    "the united states",
    "the us",
    "the usa",
    "conus",
    "lower 48",
    "the lower 48",
    "lower forty-eight",
    "contiguous us",
    "contiguous usa",
    "contiguous united states",
    "continental us",
    "continental usa",
    "continental united states",
    "anywhere in the us",
    "somewhere in the us",
];

const LEAD_PHRASES: &[&str] = &[
    "somewhere near ",
    "somewhere in ",
    "somewhere around ",
    "outside of ",
    "outside ",
    "around ",
    "near ",
    "in ",
];

/// Parse a location string into a [`Region`].
pub fn parse_region(input: &str, ctx: &RegionContext) -> Result<Region, GeoError> {
    let mut s = input.trim().trim_matches('"').trim().to_string();
    if s.is_empty() {
        return Err(GeoError::Parse("empty location".into()));
    }
    loop {
        let lower = s.to_ascii_lowercase();
        match LEAD_PHRASES
            .iter()
            .find(|p| lower.starts_with(*p) && s.len() > p.len())
        {
            Some(p) => s = s[p.len()..].trim().to_string(),
            None => break,
        }
    }
    let norm = normalize_name(&s.replace(['.'], ""));
    if CONUS_NAMES.contains(&norm.as_str()) {
        return Ok(conus_region());
    }
    if let Some(r) = parse_numeric(&s)? {
        return Ok(r);
    }
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if is_zip(&compact) {
        let z = &compact[..5];
        let rec = ctx.gaz.zcta(z)?;
        return Ok(point_region(
            format!("ZIP {z}"),
            RegionKind::Zip,
            rec.lat,
            rec.lon,
            25.0,
        ));
    }
    if norm.replace(',', "") == "washington dc" || norm.replace(',', "") == "district of columbia" {
        return Ok(states_region(&[find_state("DC").expect("DC in table")]));
    }
    // Lists of states: "NY, NJ, CT", "NY+NJ", "NY and NJ".
    let parts = split_list(&s);
    if !parts.is_empty() {
        let found: Vec<Option<&'static StateInfo>> = parts.iter().map(|p| find_state(p)).collect();
        if found.iter().all(Option::is_some) {
            let mut sts: Vec<&'static StateInfo> = Vec::new();
            for st in found.into_iter().flatten() {
                if !sts.iter().any(|x| x.abbr == st.abbr) {
                    sts.push(st);
                }
            }
            // "New York, NY" names the city, not the state twice.
            if parts.len() == 2
                && sts.len() == 1
                && let Ok(rec) = ctx.gaz.find_place(&parts[0], Some(sts[0].abbr))
            {
                return Ok(place_region(&rec));
            }
            return Ok(states_region(&sts));
        }
    }
    // "<name>, <ST>" (place or county) and "<name> <ST>" / "<name> <State Name>".
    let (name, state) = split_trailing_state(&s);
    if let Some(st) = state {
        let name = name.trim().trim_end_matches(',').trim();
        if name.is_empty() {
            return Ok(states_region(&[st]));
        }
        if let Some(cname) = strip_county_word(name) {
            let rec = ctx.gaz.find_county(&cname, st.abbr)?;
            return Ok(county_region(&rec));
        }
        match ctx.gaz.find_place(name, Some(st.abbr)) {
            Ok(rec) => return Ok(place_region(&rec)),
            Err(GeoError::NotFound(m)) => {
                // "Suffolk, NY" may mean the county.
                if let Ok(rec) = ctx.gaz.find_county(name, st.abbr) {
                    return Ok(county_region(&rec));
                }
                return Err(GeoError::NotFound(m));
            }
            Err(e) => return Err(e),
        }
    }
    // Bare name: a unique place name anywhere in the US.
    if strip_county_word(&s).is_some() {
        return Err(GeoError::Parse(format!(
            "'{s}': give the county's state, e.g. \"{s}, NY\""
        )));
    }
    let rec = ctx.gaz.find_place(&s, None)?;
    Ok(place_region(&rec))
}

/// The contiguous United States.
pub fn conus_region() -> Region {
    let bbox = states::conus_bbox();
    let sts: Vec<String> = states::STATES
        .iter()
        .filter(|s| !matches!(s.abbr, "AK" | "HI" | "PR"))
        .map(|s| s.abbr.to_string())
        .collect();
    Region {
        label: "Contiguous United States".into(),
        kind: RegionKind::Conus,
        bbox,
        center: bbox.center(),
        radius_km: None,
        utc_offset_h: states::CONUS_UTC_OFFSET_H,
        states: sts,
    }
}

fn states_region(sts: &[&'static StateInfo]) -> Region {
    let mut bbox = sts[0].bbox();
    for s in &sts[1..] {
        bbox = bbox.union(&s.bbox());
    }
    let label = if sts.len() == 1 {
        sts[0].name.to_string()
    } else {
        sts.iter().map(|s| s.abbr).collect::<Vec<_>>().join(" + ")
    };
    Region {
        label,
        kind: RegionKind::States,
        bbox,
        center: bbox.center(),
        radius_km: None,
        utc_offset_h: states::offset_union(sts.iter().copied()),
        states: sts.iter().map(|s| s.abbr.to_string()).collect(),
    }
}

fn circle_bbox(lat: f64, lon: f64, r_km: f64) -> BBox {
    let dlat = r_km / 111.32;
    let dlon = r_km / (111.32 * lat.to_radians().cos().max(0.01));
    BBox::new(
        (lat - dlat).max(-90.0),
        (lon - dlon).max(-180.0),
        (lat + dlat).min(90.0),
        (lon + dlon).min(180.0),
    )
}

fn intersects(a: &BBox, b: &BBox) -> bool {
    !(a.north < b.south || a.south > b.north || a.east < b.west || a.west > b.east)
}

fn states_touching(b: &BBox) -> Vec<&'static StateInfo> {
    states::STATES
        .iter()
        .filter(|s| intersects(&s.bbox(), b))
        .collect()
}

fn finish_offsets(bbox: &BBox, center_lon: f64) -> ((f64, f64), Vec<String>) {
    let sts = states_touching(bbox);
    if sts.is_empty() {
        (states::offset_from_longitude(center_lon), Vec::new())
    } else {
        (
            states::offset_union(sts.iter().copied()),
            sts.iter().map(|s| s.abbr.to_string()).collect(),
        )
    }
}

fn point_region(label: String, kind: RegionKind, lat: f64, lon: f64, r_km: f64) -> Region {
    let bbox = circle_bbox(lat, lon, r_km);
    let (off, sts) = finish_offsets(&bbox, lon);
    Region {
        label,
        kind,
        bbox,
        center: (lat, lon),
        radius_km: Some(r_km),
        utc_offset_h: off,
        states: sts,
    }
}

fn place_region(rec: &GazRecord) -> Region {
    let r_km = (3.0 * (rec.aland_m2 / std::f64::consts::PI).sqrt() / 1000.0).max(30.0);
    let mut r = point_region(
        format!("{}, {}", rec.base_name, rec.usps),
        RegionKind::Place,
        rec.lat,
        rec.lon,
        r_km,
    );
    prefer_state(&mut r, &rec.usps);
    r
}

fn county_region(rec: &GazRecord) -> Region {
    let r_km = (1.5 * (rec.aland_m2 / std::f64::consts::PI).sqrt() / 1000.0).max(25.0);
    let mut r = point_region(
        format!("{}, {}", rec.name, rec.usps),
        RegionKind::County,
        rec.lat,
        rec.lon,
        r_km,
    );
    prefer_state(&mut r, &rec.usps);
    r
}

/// Put the record's own state first in the state list.
fn prefer_state(r: &mut Region, usps: &str) {
    if let Some(i) = r.states.iter().position(|s| s == usps) {
        let s = r.states.remove(i);
        r.states.insert(0, s);
    } else if !usps.is_empty() {
        r.states.insert(0, usps.to_string());
    }
}

fn is_zip(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() == 5 && b.iter().all(u8::is_ascii_digit))
        || (b.len() == 10
            && b[..5].iter().all(u8::is_ascii_digit)
            && b[5] == b'-'
            && b[6..].iter().all(u8::is_ascii_digit))
}

fn split_list(s: &str) -> Vec<String> {
    let mut t = s.to_string();
    for sep in [" and ", " AND ", " And ", "&", "+", "/", ";"] {
        t = t.replace(sep, ",");
    }
    t.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Returns (rest, state) when the text ends in ", ST", " ST", " State Name" or ", State Name".
fn split_trailing_state(s: &str) -> (String, Option<&'static StateInfo>) {
    let t = s.trim().trim_end_matches(['.', ',']).trim();
    if let Some(i) = t.rfind(',') {
        let (a, b) = (&t[..i], &t[i + 1..]);
        if let Some(st) = find_state(b.trim()) {
            return (a.trim().to_string(), Some(st));
        }
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    // Longest state name first ("New York" before "York"); require >= 1 leftover word.
    for n in (1..=3usize).rev() {
        if words.len() > n {
            let tail = words[words.len() - n..].join(" ");
            if let Some(st) = find_state(&tail) {
                // A bare two-letter tail must be upper-case ("Oyster Bay NY"), so that
                // "Cove in" style words are not read as Indiana.
                if n == 1 && tail.len() == 2 && tail != tail.to_ascii_uppercase() {
                    continue;
                }
                return (words[..words.len() - n].join(" "), Some(st));
            }
        }
    }
    (t.to_string(), None)
}

/// "Suffolk County" -> Some("Suffolk"); also Parish, Borough, Census Area.
fn strip_county_word(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    for w in [
        " county",
        " parish",
        " borough",
        " census area",
        " municipality",
    ] {
        if lower.ends_with(w) && lower.len() > w.len() {
            return Some(name[..name.len() - w.len()].trim().to_string());
        }
    }
    None
}

/// Coordinates or a bbox. Returns Ok(None) when the text is not numeric.
fn parse_numeric(s: &str) -> Result<Option<Region>, GeoError> {
    let toks: Vec<&str> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if toks.len() < 2 || toks.len() > 4 {
        return Ok(None);
    }
    let mut vals = Vec::with_capacity(toks.len());
    let mut hemis = Vec::with_capacity(toks.len());
    for t in &toks {
        let lower = t.to_ascii_lowercase();
        let (body, hemi) = match lower.chars().last() {
            Some(c @ ('n' | 's' | 'e' | 'w'))
                if lower.len() > 1 && lower[..lower.len() - 1].parse::<f64>().is_ok() =>
            {
                (lower[..lower.len() - 1].to_string(), Some(c))
            }
            _ => (lower.trim_end_matches("km").to_string(), None),
        };
        let body = body.trim_end_matches('°');
        match body.parse::<f64>() {
            Ok(v) if v.is_finite() => {
                vals.push(match hemi {
                    Some('s' | 'w') => -v.abs(),
                    Some(_) => v.abs(),
                    None => v,
                });
                hemis.push(hemi);
            }
            _ => return Ok(None),
        }
    }
    // Hemisphere letters may put longitude first ("73.5W 40.8N").
    if vals.len() >= 2 && matches!(hemis[0], Some('e' | 'w')) && matches!(hemis[1], Some('n' | 's'))
    {
        vals.swap(0, 1);
    }
    let check_lat = |v: f64| (-90.0..=90.0).contains(&v);
    let check_lon = |v: f64| (-180.0..=180.0).contains(&v);
    match vals.len() {
        2 | 3 => {
            let (lat, lon) = (vals[0], vals[1]);
            if !check_lat(lat) || !check_lon(lon) {
                return Err(GeoError::Parse(format!(
                    "'{s}': latitude must be within ±90 and longitude within ±180"
                )));
            }
            let r = if vals.len() == 3 { vals[2] } else { 30.0 };
            if !(r > 0.0 && r <= 3000.0) {
                return Err(GeoError::Parse(format!(
                    "'{s}': radius {r} km out of range (0, 3000]"
                )));
            }
            let label = if vals.len() == 3 {
                format!("{lat:.4}, {lon:.4} ± {r} km")
            } else {
                format!("{lat:.4}, {lon:.4}")
            };
            Ok(Some(point_region(label, RegionKind::Point, lat, lon, r)))
        }
        4 => {
            let (south, west, north, east) = (vals[0], vals[1], vals[2], vals[3]);
            if !check_lat(south) || !check_lat(north) || !check_lon(west) || !check_lon(east) {
                return Err(GeoError::Parse(format!(
                    "'{s}': bbox must be S,W,N,E in degrees"
                )));
            }
            if south >= north || west >= east {
                return Err(GeoError::Parse(format!(
                    "'{s}': bbox must be S,W,N,E with S < N and W < E"
                )));
            }
            let bbox = BBox::new(south, west, north, east);
            let (off, sts) = finish_offsets(&bbox, bbox.center().1);
            Ok(Some(Region {
                label: format!("bbox {south},{west},{north},{east}"),
                kind: RegionKind::BBox,
                bbox,
                center: bbox.center(),
                radius_km: None,
                utc_offset_h: off,
                states: sts,
            }))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gazetteer::tests::fixture;

    fn ctx() -> RegionContext {
        RegionContext::from_gazetteer(fixture(), false)
    }

    #[test]
    fn conus_forms() {
        for q in [
            "US",
            "usa",
            "U.S.A.",
            "United States",
            "CONUS",
            "lower 48",
            "somewhere in the US",
            "near the USA",
        ] {
            let r = parse_region(q, &ctx()).unwrap_or_else(|e| panic!("{q}: {e}"));
            assert_eq!(r.kind, RegionKind::Conus, "{q}");
            assert_eq!(r.utc_offset_h, (-8.0, -4.0));
        }
    }

    #[test]
    fn states_and_lists() {
        let r = parse_region("NY", &ctx()).unwrap();
        assert_eq!((r.kind, r.label.as_str()), (RegionKind::States, "New York"));
        assert_eq!(r.utc_offset_h, (-5.0, -4.0));
        let r = parse_region("somewhere in new york", &ctx()).unwrap();
        assert_eq!(r.states, vec!["NY"]);
        let r = parse_region("NY, NJ, CT", &ctx()).unwrap();
        assert_eq!(r.states, vec!["NY", "NJ", "CT"]);
        assert!(r.bbox.south < 39.0 && r.bbox.north > 45.0);
        let r = parse_region("NY+TX", &ctx()).unwrap();
        assert_eq!(r.utc_offset_h, (-7.0, -4.0));
        let r = parse_region("Texas and Oklahoma", &ctx()).unwrap();
        assert_eq!(r.states.len(), 2);
    }

    #[test]
    fn places_counties_zips() {
        let r = parse_region("Oyster Bay, NY", &ctx()).unwrap();
        assert_eq!(r.kind, RegionKind::Place);
        assert_eq!(r.label, "Oyster Bay, NY");
        assert_eq!(r.radius_km, Some(30.0));
        assert_eq!(r.states[0], "NY");
        let r2 = parse_region("near Oyster Bay NY", &ctx()).unwrap();
        assert_eq!(r2.center, r.center);
        let r3 = parse_region("Oyster Bay New York", &ctx()).unwrap();
        assert_eq!(r3.center, r.center);
        let r = parse_region("Suffolk County, NY", &ctx()).unwrap();
        assert_eq!(r.kind, RegionKind::County);
        assert!(r.radius_km.unwrap() >= 25.0);
        let r = parse_region("Nassau, NY", &ctx()).unwrap();
        assert_eq!(r.kind, RegionKind::County);
        let r = parse_region("11771", &ctx()).unwrap();
        assert_eq!((r.kind, r.radius_km), (RegionKind::Zip, Some(25.0)));
        assert!(r.bbox.contains(40.876466, -73.524951));
        let r = parse_region("11771-1234", &ctx()).unwrap();
        assert_eq!(r.kind, RegionKind::Zip);
        assert!(matches!(
            parse_region("Springfield", &ctx()),
            Err(GeoError::Ambiguous(_))
        ));
        assert_eq!(
            parse_region("Springfield, IL", &ctx()).unwrap().states[0],
            "IL"
        );
        assert!(parse_region("Glen Cove", &ctx()).is_ok());
        let r = parse_region("New York, NY", &ctx()).unwrap();
        assert_eq!(
            (r.kind, r.label.as_str()),
            (RegionKind::Place, "New York, NY")
        );
        let r = parse_region("Washington, D.C.", &ctx()).unwrap();
        assert_eq!(r.states, vec!["DC"]);
        assert!(matches!(
            parse_region("Nowhereville, NY", &ctx()),
            Err(GeoError::NotFound(_))
        ));
    }

    #[test]
    fn numeric_forms() {
        let r = parse_region("40.87,-73.52", &ctx()).unwrap();
        assert_eq!((r.kind, r.radius_km), (RegionKind::Point, Some(30.0)));
        let r = parse_region("40.87, -73.52, 20", &ctx()).unwrap();
        assert_eq!(r.radius_km, Some(20.0));
        assert!((r.bbox.north - (40.87 + 20.0 / 111.32)).abs() < 1e-9);
        assert_eq!(r.utc_offset_h, (-5.0, -4.0));
        let r = parse_region("40.87 -73.52", &ctx()).unwrap();
        assert_eq!(r.center, (40.87, -73.52));
        let r = parse_region("40.87N 73.52W", &ctx()).unwrap();
        assert_eq!(r.center, (40.87, -73.52));
        let r = parse_region("73.52W 40.87N", &ctx()).unwrap();
        assert_eq!(r.center, (40.87, -73.52));
        let r = parse_region("40.5,-74.5,41.5,-72.0", &ctx()).unwrap();
        assert_eq!(r.kind, RegionKind::BBox);
        assert!(r.states.contains(&"NY".to_string()));
        assert!(parse_region("41.5,-72.0,40.5,-74.5", &ctx()).is_err());
        assert!(parse_region("95,-73", &ctx()).is_err());
        assert!(parse_region("40.8,-73.5,0", &ctx()).is_err());
    }

    #[test]
    fn offline_place_needs_cache() {
        let c = RegionContext::new("/nonexistent-fdop-cache", false);
        assert!(matches!(
            parse_region("Oyster Bay, NY", &c),
            Err(GeoError::Offline(_))
        ));
        assert!(parse_region("NY", &c).is_ok());
        assert!(parse_region("", &c).is_err());
    }
}
