//! US Census Bureau Gazetteer files (2024): places, ZCTAs, counties, county subdivisions.
//!
//! Each national file is a zip holding one tab-separated text file. Files are downloaded on
//! first use into `<cache_dir>/gazetteer/`, then parsed lazily (once per process). Every
//! record carries its internal point (INTPTLAT/INTPTLONG), land area and name.

use crate::GeoError;
use crate::states::normalize_name;
use fdop_types::geo::{bearing_deg, compass16, haversine_m};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const BASE_URL: &str = "https://www2.census.gov/geo/docs/maps-data/data/gazetteer/2024_Gazetteer";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GazKind {
    Place,
    Zcta,
    County,
    CountySubdivision,
}

impl GazKind {
    pub fn file_stem(self) -> &'static str {
        match self {
            GazKind::Place => "2024_Gaz_place_national",
            GazKind::Zcta => "2024_Gaz_zcta_national",
            GazKind::County => "2024_Gaz_counties_national",
            GazKind::CountySubdivision => "2024_Gaz_cousubs_national",
        }
    }
    pub fn url(self) -> String {
        format!("{BASE_URL}/{}.zip", self.file_stem())
    }
    fn idx(self) -> usize {
        match self {
            GazKind::Place => 0,
            GazKind::Zcta => 1,
            GazKind::County => 2,
            GazKind::CountySubdivision => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GazRecord {
    pub kind: GazKind,
    /// USPS state abbreviation ("" for ZCTAs).
    pub usps: String,
    pub geoid: String,
    /// Full Census NAME, e.g. "Oyster Bay CDP".
    pub name: String,
    /// NAME without its legal/statistical descriptor, e.g. "Oyster Bay".
    pub base_name: String,
    /// The stripped descriptor ("city", "town", "village", "CDP", "County", "ZCTA", ...).
    pub place_kind: String,
    /// Functional status says a government entity (FUNCSTAT not S/F/N/I).
    pub incorporated: bool,
    pub aland_m2: f64,
    pub lat: f64,
    pub lon: f64,
}

/// A place near a point, with distance and bearing FROM the place TO the point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NearbyPlace {
    pub name: String,
    pub state: String,
    pub kind: String,
    pub geoid: String,
    pub incorporated: bool,
    pub lat: f64,
    pub lon: f64,
    pub distance_m: f64,
    pub bearing_deg: f64,
    pub compass: String,
}

/// Legal / statistical descriptors appended to Census names, longest first.
const DESCRIPTORS: &[&str] = &[
    "consolidated government (balance)",
    "metropolitan government (balance)",
    "unified government (balance)",
    "metro government (balance)",
    "city and borough",
    "charter township",
    "census area",
    "municipality",
    "zona urbana",
    "comunidad",
    "plantation",
    "reservation",
    "township",
    "location",
    "purchase",
    "borough",
    "village",
    "parish",
    "county",
    "barrio",
    "grant",
    "city",
    "town",
    "gore",
    "cdp",
    "ccd",
    "ut",
];

/// Split a Census NAME into (base name, descriptor). Only one trailing descriptor is removed.
pub fn split_descriptor(name: &str) -> (String, String) {
    let lower = name.to_ascii_lowercase();
    for d in DESCRIPTORS {
        if lower.len() > d.len() + 1 && lower.ends_with(d) {
            let cut = name.len() - d.len();
            if name.as_bytes()[cut - 1] == b' ' {
                return (name[..cut - 1].trim().to_string(), name[cut..].to_string());
            }
        }
    }
    (name.trim().to_string(), String::new())
}

/// Normalised key for name matching: lowercase, no periods, descriptor stripped,
/// "saint"/"st" unified, "fort"/"ft" unified, "mount"/"mt" unified.
pub fn name_key(s: &str) -> String {
    let (base, _) = split_descriptor(s.trim());
    let n = normalize_name(&base);
    n.split(' ')
        .map(|w| match w {
            "saint" => "st",
            "fort" => "ft",
            "mount" => "mt",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse the tab-separated text of one Gazetteer file.
pub fn parse_gazetteer(kind: GazKind, text: &str) -> Result<Vec<GazRecord>, GeoError> {
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()
        .ok_or_else(|| GeoError::Data(format!("{} is empty", kind.file_stem())))?
        .trim_start_matches('\u{feff}')
        .split('\t')
        .map(|h| h.trim().to_ascii_uppercase())
        .collect();
    let col = |name: &str| header.iter().position(|h| h == name);
    let need = |name: &str| {
        col(name).ok_or_else(|| GeoError::Data(format!("{} lacks column {name}", kind.file_stem())))
    };
    let i_geoid = need("GEOID")?;
    let i_lat = need("INTPTLAT")?;
    let i_lon = need("INTPTLONG")?;
    let i_aland = need("ALAND")?;
    let i_usps = col("USPS");
    let i_name = col("NAME");
    let i_func = col("FUNCSTAT");
    let mut out = Vec::new();
    for (ln, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').map(str::trim).collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("");
        let lat: f64 = get(i_lat).parse().map_err(|_| {
            GeoError::Data(format!(
                "{} line {}: bad INTPTLAT",
                kind.file_stem(),
                ln + 2
            ))
        })?;
        let lon: f64 = get(i_lon).parse().map_err(|_| {
            GeoError::Data(format!(
                "{} line {}: bad INTPTLONG",
                kind.file_stem(),
                ln + 2
            ))
        })?;
        let geoid = get(i_geoid).to_string();
        let name = match i_name {
            Some(i) => get(i).to_string(),
            None => geoid.clone(),
        };
        let (base_name, place_kind) = if kind == GazKind::Zcta {
            (geoid.clone(), "ZCTA".to_string())
        } else {
            split_descriptor(&name)
        };
        let func = i_func.map(get).unwrap_or("");
        let incorporated = match kind {
            GazKind::Place | GazKind::CountySubdivision => {
                !func.is_empty() && !matches!(func, "S" | "F" | "N" | "I")
            }
            _ => false,
        };
        out.push(GazRecord {
            kind,
            usps: i_usps.map(get).unwrap_or("").to_string(),
            geoid,
            name,
            base_name,
            place_kind,
            incorporated,
            aland_m2: get(i_aland).parse().unwrap_or(0.0),
            lat,
            lon,
        });
    }
    Ok(out)
}

/// Lazily downloaded, lazily parsed Gazetteer tables.
pub struct Gazetteer {
    cache_dir: PathBuf,
    allow_network: bool,
    tables: [OnceLock<Result<Vec<GazRecord>, GeoError>>; 4],
}

impl Gazetteer {
    /// `cache_dir` is the fdop cache root; files go to `<cache_dir>/gazetteer/`.
    pub fn new(cache_dir: impl Into<PathBuf>, allow_network: bool) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            allow_network,
            tables: [
                OnceLock::new(),
                OnceLock::new(),
                OnceLock::new(),
                OnceLock::new(),
            ],
        }
    }

    /// Build from already-parsed records (tests, offline fixtures). Missing kinds stay empty.
    pub fn from_records(records: Vec<GazRecord>) -> Self {
        let g = Self::new(PathBuf::from("/nonexistent-fdop-gazetteer"), false);
        let mut by: [Vec<GazRecord>; 4] = Default::default();
        for r in records {
            by[r.kind.idx()].push(r);
        }
        for (i, v) in by.into_iter().enumerate() {
            let _ = g.tables[i].set(Ok(v));
        }
        g
    }

    pub fn zip_path(&self, kind: GazKind) -> PathBuf {
        self.cache_dir
            .join("gazetteer")
            .join(format!("{}.zip", kind.file_stem()))
    }

    /// Is this table available without the network (cached zip or already loaded)?
    pub fn available_offline(&self, kind: GazKind) -> bool {
        self.tables[kind.idx()].get().is_some_and(Result::is_ok) || self.zip_path(kind).exists()
    }

    pub fn records(&self, kind: GazKind) -> Result<&[GazRecord], GeoError> {
        self.tables[kind.idx()]
            .get_or_init(|| self.load(kind))
            .as_deref()
            .map_err(Clone::clone)
    }

    fn load(&self, kind: GazKind) -> Result<Vec<GazRecord>, GeoError> {
        let path = self.zip_path(kind);
        if !path.exists() {
            if !self.allow_network {
                return Err(GeoError::Offline(format!(
                    "Census gazetteer {} is not cached at {}",
                    kind.file_stem(),
                    path.display()
                )));
            }
            let net = fdop_http::Net::new()?;
            net.download_to(&kind.url(), &path)?;
        }
        let text = read_zip_text(&path)?;
        parse_gazetteer(kind, &text)
    }

    /// Find a place by name ("Oyster Bay", "Springfield city"), optionally within a state.
    /// Falls back to county subdivisions (New England / NY / NJ / PA towns). With no state
    /// and matches in several states the result is `Ambiguous`, listing the candidates.
    pub fn find_place(&self, name: &str, state: Option<&str>) -> Result<GazRecord, GeoError> {
        let key = name_key(name);
        if key.is_empty() {
            return Err(GeoError::Parse("empty place name".into()));
        }
        let st = state.map(str::to_ascii_uppercase);
        let mut cands: Vec<&GazRecord> = Vec::new();
        for kind in [GazKind::Place, GazKind::CountySubdivision] {
            let recs = self.records(kind)?;
            cands = recs
                .iter()
                .filter(|r| {
                    st.as_deref().is_none_or(|s| r.usps == s) && name_key(&r.base_name) == key
                })
                .collect();
            if !cands.is_empty() {
                break;
            }
        }
        if cands.is_empty() {
            let where_ = st.map(|s| format!(" in {s}")).unwrap_or_default();
            return Err(GeoError::NotFound(format!(
                "no Census place or town named '{name}'{where_}"
            )));
        }
        let mut states: Vec<&str> = cands.iter().map(|r| r.usps.as_str()).collect();
        states.sort_unstable();
        states.dedup();
        if states.len() > 1 {
            let mut list: Vec<String> = cands
                .iter()
                .map(|r| format!("{}, {}", r.name, r.usps))
                .collect();
            list.sort();
            list.dedup();
            let shown = list.iter().take(12).cloned().collect::<Vec<_>>().join("; ");
            let more = if list.len() > 12 {
                format!(" (+{} more)", list.len() - 12)
            } else {
                String::new()
            };
            return Err(GeoError::Ambiguous(format!(
                "'{name}' matches places in {} states: {shown}{more}; add a state, e.g. \"{name}, {}\"",
                states.len(),
                states[0]
            )));
        }
        cands.sort_by(|a, b| {
            b.incorporated.cmp(&a.incorporated).then(
                b.aland_m2
                    .partial_cmp(&a.aland_m2)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        Ok(cands[0].clone())
    }

    /// Find a county (or parish / borough / census area) within a state.
    pub fn find_county(&self, name: &str, state: &str) -> Result<GazRecord, GeoError> {
        let key = name_key(name);
        let st = state.to_ascii_uppercase();
        let recs = self.records(GazKind::County)?;
        recs.iter()
            .filter(|r| r.usps == st && name_key(&r.base_name) == key)
            .max_by(|a, b| {
                a.aland_m2
                    .partial_cmp(&b.aland_m2)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .ok_or_else(|| GeoError::NotFound(format!("no county named '{name}' in {st}")))
    }

    /// ZIP Code Tabulation Area by 5-digit code.
    pub fn zcta(&self, zip: &str) -> Result<GazRecord, GeoError> {
        let z = zip.trim();
        self.records(GazKind::Zcta)?
            .iter()
            .find(|r| r.geoid == z)
            .cloned()
            .ok_or_else(|| {
                GeoError::NotFound(format!(
                    "ZIP {z} is not a Census ZCTA (PO-box-only ZIPs have no area)"
                ))
            })
    }

    /// The `k` places (incorporated places and CDPs) whose internal points are nearest.
    pub fn nearest_places(
        &self,
        lat: f64,
        lon: f64,
        k: usize,
    ) -> Result<Vec<NearbyPlace>, GeoError> {
        let recs = self.records(GazKind::Place)?;
        Ok(nearest_of(recs, lat, lon, k))
    }

    /// Nearest ZCTA internal point and its distance (m). A centroid proxy, not containment.
    pub fn nearest_zcta(&self, lat: f64, lon: f64) -> Result<(GazRecord, f64), GeoError> {
        let recs = self.records(GazKind::Zcta)?;
        recs.iter()
            .map(|r| (r, haversine_m(lat, lon, r.lat, r.lon)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(r, d)| (r.clone(), d))
            .ok_or_else(|| GeoError::NotFound("ZCTA table is empty".into()))
    }
}

/// k nearest records by haversine distance, with bearing from the record to the point.
pub fn nearest_of(recs: &[GazRecord], lat: f64, lon: f64, k: usize) -> Vec<NearbyPlace> {
    // Cheap pre-filter on an equirectangular distance, then exact haversine on the shortlist.
    let cosl = lat.to_radians().cos();
    let mut scored: Vec<(f64, &GazRecord)> = recs
        .iter()
        .map(|r| {
            let dy = r.lat - lat;
            let dx = (r.lon - lon) * cosl;
            (dx * dx + dy * dy, r)
        })
        .collect();
    let take = (k * 4).max(16).min(scored.len());
    if take > 0 && take < scored.len() {
        scored.select_nth_unstable_by(take - 1, |a, b| {
            a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(take);
    }
    let mut exact: Vec<(f64, &GazRecord)> = scored
        .into_iter()
        .map(|(_, r)| (haversine_m(r.lat, r.lon, lat, lon), r))
        .collect();
    exact.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    exact
        .into_iter()
        .take(k)
        .map(|(d, r)| {
            let brg = bearing_deg(r.lat, r.lon, lat, lon);
            NearbyPlace {
                name: r.base_name.clone(),
                state: r.usps.clone(),
                kind: r.place_kind.clone(),
                geoid: r.geoid.clone(),
                incorporated: r.incorporated,
                lat: r.lat,
                lon: r.lon,
                distance_m: d,
                bearing_deg: brg,
                compass: compass16(brg).to_string(),
            }
        })
        .collect()
}

fn read_zip_text(path: &Path) -> Result<String, GeoError> {
    let f = std::fs::File::open(path)?;
    let mut za =
        zip::ZipArchive::new(f).map_err(|e| GeoError::Data(format!("{}: {e}", path.display())))?;
    for i in 0..za.len() {
        let mut entry = za
            .by_index(i)
            .map_err(|e| GeoError::Data(format!("{}: {e}", path.display())))?;
        if entry.name().ends_with(".txt") {
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            // Census files are Latin-1 / UTF-8 mixed; map invalid bytes as Latin-1.
            return Ok(match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
            });
        }
    }
    Err(GeoError::Data(format!(
        "{} holds no .txt file",
        path.display()
    )))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PLACES: &str = "USPS\tGEOID\tANSICODE\tNAME\tLSAD\tFUNCSTAT\tALAND\tAWATER\tALAND_SQMI\tAWATER_SQMI\tINTPTLAT\tINTPTLONG                                     \n\
NY\t3655992\t02390185\tOyster Bay CDP\t57\tS\t4523214\t1234\t1.746\t0.0\t40.866940\t-73.531318\n\
NY\t3655970\t00979360\tOyster Bay Cove village\t47\tA\t10789560\t1000\t4.166\t0.1\t40.865400\t-73.509500\n\
NY\t3629113\t00979014\tGlen Cove city\t25\tA\t17234000\t5000\t6.654\t0.1\t40.867600\t-73.632400\n\
NY\t3651055\t02390140\tNew York city\t25\tA\t778000000\t5000\t300.4\t0.1\t40.663100\t-73.938700\n\
IL\t1772000\t02395940\tSpringfield city\t25\tA\t160000000\t5000\t61.7\t0.1\t39.784000\t-89.646000\n\
MA\t2567000\t00619428\tSpringfield city\t25\tA\t82000000\t5000\t31.9\t0.1\t42.115000\t-72.540000\n\
MO\t2970000\t02395942\tSt. Louis city\t25\tA\t160000000\t5000\t61.7\t0.1\t38.635000\t-90.244000\n";

    pub const ZCTAS: &str = "GEOID\tALAND\tAWATER\tALAND_SQMI\tAWATER_SQMI\tINTPTLAT\tINTPTLONG     \n\
11771\t25000000\t1000\t9.65\t0.0\t40.874000\t-73.523000\n\
11542\t17000000\t1000\t6.56\t0.0\t40.870000\t-73.627000\n";

    pub const COUNTIES: &str = "USPS\tGEOID\tANSICODE\tNAME\tALAND\tAWATER\tALAND_SQMI\tAWATER_SQMI\tINTPTLAT\tINTPTLONG   \n\
NY\t36059\t00974128\tNassau County\t736000000\t441000000\t284.2\t170.0\t40.729000\t-73.589000\n\
NY\t36103\t00974149\tSuffolk County\t2360000000\t3800000000\t911.0\t1468.0\t40.943000\t-72.692000\n\
LA\t22071\t00558530\tOrleans Parish\t439000000\t467000000\t169.4\t180.0\t30.069000\t-89.931000\n";

    pub const COUSUBS: &str = "USPS\tGEOID\tANSICODE\tNAME\tFUNCSTAT\tALAND\tAWATER\tALAND_SQMI\tAWATER_SQMI\tINTPTLAT\tINTPTLONG\n\
NY\t3605956000\t00979333\tOyster Bay town\tA\t268000000\t2000\t103.5\t0.1\t40.800000\t-73.500000\n\
VT\t5000710675\t01462073\tStowe town\tA\t187000000\t2000\t72.2\t0.1\t44.465000\t-72.700000\n";

    pub fn fixture() -> Gazetteer {
        let mut recs = parse_gazetteer(GazKind::Place, PLACES).unwrap();
        recs.extend(parse_gazetteer(GazKind::Zcta, ZCTAS).unwrap());
        recs.extend(parse_gazetteer(GazKind::County, COUNTIES).unwrap());
        recs.extend(parse_gazetteer(GazKind::CountySubdivision, COUSUBS).unwrap());
        Gazetteer::from_records(recs)
    }

    #[test]
    fn parses_and_strips_descriptors() {
        let p = parse_gazetteer(GazKind::Place, PLACES).unwrap();
        assert_eq!(p.len(), 7);
        assert_eq!(p[0].base_name, "Oyster Bay");
        assert_eq!(p[0].place_kind, "CDP");
        assert!(!p[0].incorporated);
        assert_eq!(p[1].base_name, "Oyster Bay Cove");
        assert!(p[1].incorporated);
        let z = parse_gazetteer(GazKind::Zcta, ZCTAS).unwrap();
        assert_eq!(z[0].geoid, "11771");
        assert_eq!(
            split_descriptor("Juneau city and borough"),
            ("Juneau".into(), "city and borough".into())
        );
        assert_eq!(split_descriptor("Orleans Parish").0, "Orleans");
        assert_eq!(split_descriptor("Townsend").0, "Townsend");
    }

    #[test]
    fn lookups() {
        let g = fixture();
        assert_eq!(
            g.find_place("oyster bay", Some("NY")).unwrap().geoid,
            "3655992"
        );
        assert_eq!(g.find_place("Glen Cove City", None).unwrap().usps, "NY");
        assert_eq!(
            g.find_place("Saint Louis", Some("MO")).unwrap().geoid,
            "2970000"
        );
        assert!(matches!(
            g.find_place("Springfield", None),
            Err(GeoError::Ambiguous(_))
        ));
        assert_eq!(g.find_place("Springfield", Some("ma")).unwrap().usps, "MA");
        assert_eq!(
            g.find_place("Stowe", Some("VT")).unwrap().kind,
            GazKind::CountySubdivision
        );
        assert!(matches!(
            g.find_place("Nowhereville", Some("NY")),
            Err(GeoError::NotFound(_))
        ));
        assert_eq!(g.find_county("Nassau", "NY").unwrap().geoid, "36059");
        assert_eq!(
            g.find_county("orleans parish", "la").unwrap().geoid,
            "22071"
        );
        assert_eq!(g.zcta("11771").unwrap().lat, 40.874);
        let near = g.nearest_places(40.876466, -73.524951, 2).unwrap();
        assert_eq!(near[0].name, "Oyster Bay");
        assert_eq!(near[1].name, "Oyster Bay Cove");
        assert!(near[0].distance_m < 1500.0);
        assert_eq!(near[0].compass, "NNE"); // from the CDP centroid to the point
        let (z, d) = g.nearest_zcta(40.876466, -73.524951).unwrap();
        assert_eq!(z.geoid, "11771");
        assert!(d < 500.0);
    }

    #[test]
    fn offline_without_cache_errors() {
        let g = Gazetteer::new("/nonexistent-fdop-cache", false);
        assert!(matches!(g.zcta("11771"), Err(GeoError::Offline(_))));
    }
}
