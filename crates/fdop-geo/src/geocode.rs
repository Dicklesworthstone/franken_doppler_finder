//! Reverse geocoding: Census Geocoder API (authoritative containment: place, town, county,
//! ZCTA), with an offline fallback to Gazetteer internal points (nearest-centroid proxies).

use crate::GeoError;
use crate::gazetteer::{GazKind, GazRecord, Gazetteer, NearbyPlace, nearest_of};
use crate::states::{find_state, states_containing};
use fdop_types::geo::{compass16, haversine_m};
use serde::{Deserialize, Serialize};

/// Census "geographies/coordinates" result for one point (containment, not proximity).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CensusGeo {
    pub state: Option<String>,
    pub state_abbr: Option<String>,
    pub state_fips: Option<String>,
    /// Full county name, e.g. "Nassau County".
    pub county: Option<String>,
    pub county_geoid: Option<String>,
    /// Full county-subdivision name, e.g. "Oyster Bay town".
    pub county_subdivision: Option<String>,
    /// Incorporated place or CDP base name containing the point, e.g. "Oyster Bay".
    pub place: Option<String>,
    /// Descriptor of `place`: "city", "village", "CDP", ...
    pub place_kind: Option<String>,
    pub place_geoid: Option<String>,
    /// 2020 ZIP Code Tabulation Area containing the point.
    pub zcta: Option<String>,
    /// Layer names the API returned (for provenance).
    pub layers: Vec<String>,
}

pub const CENSUS_URL: &str = "https://geocoding.geo.census.gov/geocoder/geographies/coordinates";

pub fn reverse_url(lat: f64, lon: f64) -> String {
    format!(
        "{CENSUS_URL}?x={lon:.6}&y={lat:.6}&benchmark=Public_AR_Current&vintage=Current_Current&format=json&layers=all"
    )
}

/// Query the Census Geocoder for the geographies containing (lat, lon).
pub fn reverse(net: &fdop_http::Net, lat: f64, lon: f64) -> Result<CensusGeo, GeoError> {
    let resp = net.get(&reverse_url(lat, lon), &[("Accept", "application/json")])?;
    let v = resp.json().map_err(GeoError::Data)?;
    parse_census_geographies(&v)
}

/// Parse the API's JSON (`result.geographies`). Layer keys vary by vintage, so ZCTAs and
/// places are matched by substring.
pub fn parse_census_geographies(v: &serde_json::Value) -> Result<CensusGeo, GeoError> {
    let geos = v
        .pointer("/result/geographies")
        .and_then(|g| g.as_object())
        .ok_or_else(|| GeoError::Data("Census response lacks result.geographies".into()))?;
    let first =
        |pred: &dyn Fn(&str) -> bool| -> Option<&serde_json::Map<String, serde_json::Value>> {
            geos.iter().filter(|(k, _)| pred(k)).find_map(|(_, arr)| {
                arr.as_array()
                    .and_then(|a| a.first())
                    .and_then(|r| r.as_object())
            })
        };
    let field =
        |rec: Option<&serde_json::Map<String, serde_json::Value>>, key: &str| -> Option<String> {
            rec.and_then(|r| r.get(key))
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
    let st = first(&|k| k == "States");
    let co = first(&|k| k == "Counties");
    let cs = first(&|k| k == "County Subdivisions");
    let inc = first(&|k| k == "Incorporated Places");
    let cdp = first(&|k| k == "Census Designated Places");
    let zc = first(&|k| k.contains("ZIP Code Tabulation"));
    let place_rec = inc.or(cdp);
    let (place, place_kind) = match place_rec {
        Some(_) => {
            let base = field(place_rec, "BASENAME");
            let name = field(place_rec, "NAME").unwrap_or_default();
            let kind = base
                .as_ref()
                .and_then(|b| name.strip_prefix(b.as_str()))
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty());
            (
                base,
                kind.or_else(|| {
                    Some(if inc.is_some() {
                        "place".into()
                    } else {
                        "CDP".into()
                    })
                }),
            )
        }
        None => (None, None),
    };
    let zcta = field(zc, "ZCTA5")
        .or_else(|| field(zc, "BASENAME"))
        .or_else(|| field(zc, "GEOID"));
    let mut layers: Vec<String> = geos.keys().cloned().collect();
    layers.sort();
    Ok(CensusGeo {
        state: field(st, "NAME"),
        state_abbr: field(st, "STUSAB"),
        state_fips: field(st, "GEOID"),
        county: field(co, "NAME"),
        county_geoid: field(co, "GEOID"),
        county_subdivision: field(cs, "NAME"),
        place,
        place_kind,
        place_geoid: field(place_rec, "GEOID"),
        zcta,
        layers,
    })
}

/// What the report says about a location.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaceReport {
    /// e.g. "Oyster Bay, NY 11771 (Nassau County)".
    pub headline: String,
    pub lat: f64,
    pub lon: f64,
    /// Incorporated place or CDP containing the point (Census API), or an approximate
    /// gazetteer match (see `notes`).
    pub place: Option<String>,
    pub place_kind: Option<String>,
    pub county_subdivision: Option<String>,
    pub county: Option<String>,
    pub state: Option<String>,
    pub state_abbr: Option<String>,
    pub zip: Option<String>,
    /// "census-api" (containing ZCTA) or "gazetteer-nearest-zcta-centroid".
    pub zip_source: String,
    /// "census-api" or "gazetteer".
    pub source: String,
    /// The five nearest places (internal points), distance/bearing from place to point.
    pub nearest_places: Vec<NearbyPlace>,
    pub notes: Vec<String>,
}

/// Describe (lat, lon): Census API when `net` is given and answers, else the gazetteer.
/// Never fails: missing pieces are `None` and explained in `notes`.
pub fn describe_location(
    net: Option<&fdop_http::Net>,
    gaz: &Gazetteer,
    lat: f64,
    lon: f64,
) -> PlaceReport {
    let mut rep = PlaceReport {
        lat,
        lon,
        ..Default::default()
    };
    match gaz.nearest_places(lat, lon, 5) {
        Ok(v) => rep.nearest_places = v,
        Err(e) => rep.notes.push(format!("nearest places unavailable: {e}")),
    }
    let api = match net {
        Some(n) => match reverse(n, lat, lon) {
            Ok(g) => Some(g),
            Err(e) => {
                rep.notes.push(format!(
                    "Census Geocoder failed ({e}); using gazetteer centroids"
                ));
                None
            }
        },
        None => {
            rep.notes
                .push("offline: using gazetteer centroids, not containment".into());
            None
        }
    };
    if let Some(g) = api.filter(|g| g.state.is_some()) {
        rep.source = "census-api".into();
        rep.place = g.place;
        rep.place_kind = g.place_kind;
        rep.county_subdivision = g.county_subdivision;
        rep.county = g.county;
        rep.state = g.state;
        rep.state_abbr = g.state_abbr;
        if let Some(z) = g.zcta {
            rep.zip = Some(z);
            rep.zip_source = "census-api".into();
        } else {
            rep.notes
                .push("point lies in no ZCTA (water or unpopulated land)".into());
        }
    } else {
        rep.source = "gazetteer".into();
        fill_from_gazetteer(&mut rep, gaz, lat, lon);
    }
    if rep.zip.is_none()
        && let Ok((z, d)) = gaz.nearest_zcta(lat, lon)
    {
        rep.zip = Some(z.geoid.clone());
        rep.zip_source = "gazetteer-nearest-zcta-centroid".into();
        rep.notes.push(format!(
            "ZIP {} is the nearest ZCTA centroid ({:.1} km away), not a containment test",
            z.geoid,
            d / 1000.0
        ));
    }
    rep.headline = headline(&rep);
    rep
}

/// Name a second county when its size-scaled distance is within this factor of the best one's.
/// Over all 36,291 county-subdivision internal points in the 2024 Gazetteer, this names two
/// counties for 33% of points and leaves 2.7% wrong. Plain nearest centroid was wrong for 11.8%.
const SECOND_COUNTY_RATIO: f64 = 1.5;

/// The county a point most likely lies in, or two when it is too close to call. Distance is
/// scaled by each county's equal-area radius: a point can sit far from a large county's centroid
/// and still be inside it, while lying nearer the centroid of a small neighbour.
fn likely_county(recs: &[&GazRecord], lat: f64, lon: f64) -> Option<String> {
    let mut scored: Vec<(f64, &GazRecord)> = recs
        .iter()
        .map(|&r| {
            let r_eq = (r.aland_m2.max(1.0) / std::f64::consts::PI).sqrt();
            (haversine_m(r.lat, r.lon, lat, lon) / r_eq, r)
        })
        .collect();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    let label = |r: &GazRecord| {
        format!("{} {}", r.base_name, r.place_kind)
            .trim()
            .to_string()
    };
    match scored.as_slice() {
        [] => None,
        [(best, a), (next, b), ..] if *next < SECOND_COUNTY_RATIO * *best => {
            Some(format!("{} or {}", label(a), label(b)))
        }
        [(_, a), ..] => Some(label(a)),
    }
}

fn fill_from_gazetteer(rep: &mut PlaceReport, gaz: &Gazetteer, lat: f64, lon: f64) {
    // State: the nearest place's state when it is close, else the containing state box.
    let near_state = rep
        .nearest_places
        .first()
        .filter(|p| p.distance_m < 30_000.0)
        .map(|p| p.state.clone());
    let st = near_state
        .as_deref()
        .and_then(find_state)
        .or_else(|| states_containing(lat, lon).into_iter().next());
    if let Some(s) = st {
        rep.state = Some(s.name.to_string());
        rep.state_abbr = Some(s.abbr.to_string());
    }
    if let Some(p) = rep.nearest_places.first() {
        // Treat the point as "in" the place when within the place's equal-area radius.
        let aland = gaz
            .records(GazKind::Place)
            .ok()
            .and_then(|r| r.iter().find(|x| x.geoid == p.geoid))
            .map_or(0.0, |x| x.aland_m2);
        let r_eq = (aland / std::f64::consts::PI).sqrt();
        if p.distance_m <= r_eq.max(1000.0) {
            rep.place = Some(p.name.clone());
            rep.place_kind = Some(p.kind.clone());
            rep.notes.push(format!(
                "place '{}' inferred from its gazetteer centroid ({:.1} km), approximate",
                p.name,
                p.distance_m / 1000.0
            ));
        }
    }
    if let Ok(recs) = gaz.records(GazKind::County) {
        let in_state: Vec<_> = recs
            .iter()
            .filter(|r| rep.state_abbr.as_deref().is_none_or(|s| r.usps == s))
            .collect();
        if let Some(county) = likely_county(&in_state, lat, lon) {
            rep.county = Some(county);
            rep.notes.push(
                "county estimated from gazetteer centroids, not containment; two names mean too \
                 close to call"
                    .into(),
            );
        }
    }
    if let Ok(recs) = gaz.records(GazKind::CountySubdivision) {
        let in_state: Vec<_> = recs
            .iter()
            .filter(|r| rep.state_abbr.as_deref().is_none_or(|s| r.usps == s))
            .cloned()
            .collect();
        if let Some(c) = nearest_of(&in_state, lat, lon, 1).into_iter().next() {
            rep.county_subdivision = Some(format!("{} {}", c.name, c.kind).trim().to_string());
        }
    }
}

/// "Oyster Bay, NY 11771 (Nassau County)", or "3.2 km ESE of Glen Cove, NY 11542 (...)".
pub fn headline(rep: &PlaceReport) -> String {
    let st = rep
        .state_abbr
        .clone()
        .or_else(|| rep.state.clone())
        .unwrap_or_default();
    let name = if let Some(p) = &rep.place {
        p.clone()
    } else if let Some(cs) = &rep.county_subdivision {
        let (base, kind) = crate::gazetteer::split_descriptor(cs);
        if kind.is_empty() || kind.eq_ignore_ascii_case("CCD") {
            base
        } else {
            format!("{base} ({kind})")
        }
    } else if let Some(n) = rep.nearest_places.first() {
        format!(
            "{:.1} km {} of {}",
            n.distance_m / 1000.0,
            compass16(n.bearing_deg),
            n.name
        )
    } else {
        format!("{:.5}, {:.5}", rep.lat, rep.lon)
    };
    let mut h = name;
    if !st.is_empty() {
        h.push_str(", ");
        h.push_str(&st);
    }
    if let Some(z) = &rep.zip {
        h.push(' ');
        h.push_str(z);
    }
    if let Some(c) = &rep.county {
        h.push_str(&format!(" ({c})"));
    }
    h
}

/// Distance (m) from (lat, lon) to the nearest listed place, if any.
pub fn nearest_distance_m(rep: &PlaceReport) -> Option<f64> {
    rep.nearest_places
        .first()
        .map(|p| haversine_m(p.lat, p.lon, rep.lat, rep.lon))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gazetteer::tests::fixture;

    const SAMPLE: &str = r#"{"result":{"input":{},"geographies":{
      "States":[{"NAME":"New York","STUSAB":"NY","GEOID":"36"}],
      "Counties":[{"NAME":"Nassau County","GEOID":"36059","BASENAME":"Nassau"}],
      "County Subdivisions":[{"NAME":"Oyster Bay town","GEOID":"3605956000","BASENAME":"Oyster Bay"}],
      "Census Designated Places":[{"NAME":"Oyster Bay CDP","GEOID":"3655992","BASENAME":"Oyster Bay"}],
      "2020 Census ZIP Code Tabulation Areas":[{"NAME":"ZCTA5 11771","ZCTA5":"11771","GEOID":"11771","BASENAME":"11771"}]
    }}}"#;

    #[test]
    fn parses_api_payload() {
        let v: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        let g = parse_census_geographies(&v).unwrap();
        assert_eq!(g.state_abbr.as_deref(), Some("NY"));
        assert_eq!(g.county.as_deref(), Some("Nassau County"));
        assert_eq!(g.place.as_deref(), Some("Oyster Bay"));
        assert_eq!(g.place_kind.as_deref(), Some("CDP"));
        assert_eq!(g.zcta.as_deref(), Some("11771"));
        assert_eq!(g.county_subdivision.as_deref(), Some("Oyster Bay town"));
        let rep = PlaceReport {
            place: g.place,
            state_abbr: g.state_abbr,
            zip: g.zcta,
            county: g.county,
            ..Default::default()
        };
        assert_eq!(headline(&rep), "Oyster Bay, NY 11771 (Nassau County)");
    }

    #[test]
    fn offline_fallback_uses_gazetteer() {
        let g = fixture();
        let rep = describe_location(None, &g, 40.876466, -73.524951);
        assert_eq!(rep.source, "gazetteer");
        assert_eq!(rep.state_abbr.as_deref(), Some("NY"));
        assert_eq!(rep.zip.as_deref(), Some("11771"));
        assert_eq!(rep.zip_source, "gazetteer-nearest-zcta-centroid");
        assert_eq!(rep.county.as_deref(), Some("Nassau County"));
        assert_eq!(rep.nearest_places.len(), 5);
        assert!(rep.headline.contains("NY 11771"), "{}", rep.headline);
        assert!(!rep.notes.is_empty());
    }

    /// A point nearer a small county's centroid than a large neighbour's is not thereby in the
    /// small county. Plain nearest centroid named Nassau alone for the first point.
    #[test]
    fn offline_county_names_both_when_too_close_to_call() {
        let g = fixture();
        let close = describe_location(None, &g, 40.76, -73.22);
        assert_eq!(
            close.county.as_deref(),
            Some("Suffolk County or Nassau County")
        );
        assert!(
            close
                .headline
                .ends_with("(Suffolk County or Nassau County)"),
            "{}",
            close.headline
        );
        let inside = describe_location(None, &g, 40.95, -72.7);
        assert_eq!(inside.county.as_deref(), Some("Suffolk County"));
        assert_eq!(likely_county(&[], 40.95, -72.7), None);
        let recs = g.records(GazKind::County).unwrap();
        assert_eq!(
            likely_county(&[&recs[2]], 40.95, -72.7).as_deref(),
            Some("Orleans Parish"),
            "a lone county is named however far away"
        );
    }

    #[test]
    fn headline_without_place() {
        let rep = PlaceReport {
            county_subdivision: Some("Oyster Bay town".into()),
            state_abbr: Some("NY".into()),
            ..Default::default()
        };
        assert_eq!(headline(&rep), "Oyster Bay (town), NY");
    }
}
