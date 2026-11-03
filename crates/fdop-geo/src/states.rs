//! Embedded US state table: abbreviation, name, FIPS, bounding box and UTC-offset range.
//!
//! Bounding boxes come from the Census cartographic boundary file `cb_2023_us_state_20m`
//! (public domain). Alaska's box is clipped to the western hemisphere (the Aleutians cross
//! the antimeridian). The UTC-offset range covers every zone used in the state, standard and
//! daylight time, so a local calendar date always maps inside it.

use fdop_types::BBox;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateInfo {
    pub abbr: &'static str,
    pub name: &'static str,
    pub fips: &'static str,
    /// [south, west, north, east]
    pub bbox: [f64; 4],
    /// (most negative, least negative) UTC offset in hours, DST included.
    pub utc_offset_h: (f64, f64),
}

impl StateInfo {
    pub fn bbox(&self) -> BBox {
        BBox::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3])
    }
}

/// Contiguous United States (lower 48 + DC).
pub const CONUS_BBOX: [f64; 4] = [24.4, -125.0, 49.5, -66.9];
/// UTC offsets spanned by the contiguous US (PST .. EDT).
pub const CONUS_UTC_OFFSET_H: (f64, f64) = (-8.0, -4.0);

pub fn conus_bbox() -> BBox {
    BBox::new(CONUS_BBOX[0], CONUS_BBOX[1], CONUS_BBOX[2], CONUS_BBOX[3])
}

const fn s(
    abbr: &'static str,
    name: &'static str,
    fips: &'static str,
    bbox: [f64; 4],
    lo: f64,
    hi: f64,
) -> StateInfo {
    StateInfo {
        abbr,
        name,
        fips,
        bbox,
        utc_offset_h: (lo, hi),
    }
}

const E: (f64, f64) = (-5.0, -4.0); // Eastern only
const EC: (f64, f64) = (-6.0, -4.0); // Eastern + Central
const C: (f64, f64) = (-6.0, -5.0); // Central only
const CM: (f64, f64) = (-7.0, -5.0); // Central + Mountain
const M: (f64, f64) = (-7.0, -6.0); // Mountain only
const MP: (f64, f64) = (-8.0, -6.0); // Mountain + Pacific
const P: (f64, f64) = (-8.0, -7.0); // Pacific only

pub static STATES: [StateInfo; 52] = [
    s(
        "AK",
        "Alaska",
        "02",
        [51.2, -179.2, 71.4, -129.9],
        -10.0,
        -8.0,
    ),
    s(
        "AL",
        "Alabama",
        "01",
        [30.2283, -88.4687, 35.0080, -84.8918],
        C.0,
        C.1,
    ),
    s(
        "AR",
        "Arkansas",
        "05",
        [33.0041, -94.6179, 36.4996, -89.6473],
        C.0,
        C.1,
    ),
    // Arizona: MST all year, but the Navajo Nation observes DST.
    s(
        "AZ",
        "Arizona",
        "04",
        [31.3322, -114.8142, 37.0032, -109.0452],
        M.0,
        M.1,
    ),
    s(
        "CA",
        "California",
        "06",
        [32.5342, -124.4096, 42.0092, -114.1391],
        P.0,
        P.1,
    ),
    s(
        "CO",
        "Colorado",
        "08",
        [36.9924, -109.0601, 41.0031, -102.0419],
        M.0,
        M.1,
    ),
    s(
        "CT",
        "Connecticut",
        "09",
        [40.9852, -73.7278, 42.0496, -71.7894],
        E.0,
        E.1,
    ),
    s(
        "DC",
        "District of Columbia",
        "11",
        [38.7916, -77.1198, 38.9955, -76.9094],
        E.0,
        E.1,
    ),
    s(
        "DE",
        "Delaware",
        "10",
        [38.4510, -75.7886, 39.8392, -75.0489],
        E.0,
        E.1,
    ),
    s(
        "FL",
        "Florida",
        "12",
        [24.4981, -87.6349, 31.0007, -80.0321],
        EC.0,
        EC.1,
    ),
    s(
        "GA",
        "Georgia",
        "13",
        [30.3608, -85.6052, 35.0007, -80.8431],
        E.0,
        E.1,
    ),
    s(
        "HI",
        "Hawaii",
        "15",
        [18.9175, -160.2496, 22.2326, -154.8144],
        -10.0,
        -10.0,
    ),
    s(
        "IA",
        "Iowa",
        "19",
        [40.3783, -96.6247, 43.5009, -90.1406],
        C.0,
        C.1,
    ),
    s(
        "ID",
        "Idaho",
        "16",
        [41.9886, -117.2430, 49.0009, -111.0436],
        MP.0,
        MP.1,
    ),
    s(
        "IL",
        "Illinois",
        "17",
        [36.9821, -91.5120, 42.5085, -87.4989],
        C.0,
        C.1,
    ),
    s(
        "IN",
        "Indiana",
        "18",
        [37.7676, -88.0674, 41.7602, -84.7864],
        EC.0,
        EC.1,
    ),
    s(
        "KS",
        "Kansas",
        "20",
        [36.9931, -102.0517, 40.0031, -94.5919],
        CM.0,
        CM.1,
    ),
    s(
        "KY",
        "Kentucky",
        "21",
        [36.4979, -89.5715, 39.1474, -81.9683],
        EC.0,
        EC.1,
    ),
    s(
        "LA",
        "Louisiana",
        "22",
        [28.9338, -94.0431, 33.0195, -89.0143],
        C.0,
        C.1,
    ),
    s(
        "MA",
        "Massachusetts",
        "25",
        [41.2380, -73.5081, 42.8846, -69.9283],
        E.0,
        E.1,
    ),
    s(
        "MD",
        "Maryland",
        "24",
        [37.9168, -79.4869, 39.7231, -75.0489],
        E.0,
        E.1,
    ),
    s(
        "ME",
        "Maine",
        "23",
        [43.0598, -71.0839, 47.4572, -66.9499],
        E.0,
        E.1,
    ),
    s(
        "MI",
        "Michigan",
        "26",
        [41.6961, -90.4181, 48.2107, -82.4159],
        EC.0,
        EC.1,
    ),
    s(
        "MN",
        "Minnesota",
        "27",
        [43.4995, -97.2290, 49.3844, -89.4892],
        C.0,
        C.1,
    ),
    s(
        "MO",
        "Missouri",
        "29",
        [35.9958, -95.7656, 40.6136, -89.0988],
        C.0,
        C.1,
    ),
    s(
        "MS",
        "Mississippi",
        "28",
        [30.1808, -91.6444, 34.9960, -88.0979],
        C.0,
        C.1,
    ),
    s(
        "MT",
        "Montana",
        "30",
        [44.3803, -116.0492, 49.0014, -104.0391],
        M.0,
        M.1,
    ),
    s(
        "NC",
        "North Carolina",
        "37",
        [33.8511, -84.3219, 36.5881, -75.4587],
        E.0,
        E.1,
    ),
    s(
        "ND",
        "North Dakota",
        "38",
        [45.9352, -104.0489, 49.0007, -96.5545],
        CM.0,
        CM.1,
    ),
    s(
        "NE",
        "Nebraska",
        "31",
        [40.0000, -104.0532, 43.0008, -95.3083],
        CM.0,
        CM.1,
    ),
    s(
        "NH",
        "New Hampshire",
        "33",
        [42.6970, -72.5561, 45.3055, -70.7038],
        E.0,
        E.1,
    ),
    s(
        "NJ",
        "New Jersey",
        "34",
        [38.9285, -75.5594, 41.3574, -73.8940],
        E.0,
        E.1,
    ),
    s(
        "NM",
        "New Mexico",
        "35",
        [31.3323, -109.0500, 37.0001, -103.0020],
        M.0,
        M.1,
    ),
    // Nevada: Pacific, except West Wendover (Mountain).
    s(
        "NV",
        "Nevada",
        "32",
        [35.0019, -120.0051, 42.0004, -114.0399],
        MP.0,
        MP.1,
    ),
    s(
        "NY",
        "New York",
        "36",
        [40.5024, -79.7621, 45.0147, -71.8562],
        E.0,
        E.1,
    ),
    s(
        "OH",
        "Ohio",
        "39",
        [38.4043, -84.8202, 41.9775, -80.5189],
        E.0,
        E.1,
    ),
    s(
        "OK",
        "Oklahoma",
        "40",
        [33.6218, -103.0025, 37.0016, -94.4315],
        C.0,
        C.1,
    ),
    s(
        "OR",
        "Oregon",
        "41",
        [41.9927, -124.5524, 46.2691, -116.4635],
        MP.0,
        MP.1,
    ),
    s(
        "PA",
        "Pennsylvania",
        "42",
        [39.7201, -80.5199, 42.2699, -74.6949],
        E.0,
        E.1,
    ),
    // Puerto Rico: Atlantic Standard Time, no DST.
    s(
        "PR",
        "Puerto Rico",
        "72",
        [17.9138, -67.9558, 18.5117, -65.2216],
        -4.0,
        -4.0,
    ),
    s(
        "RI",
        "Rhode Island",
        "44",
        [41.1463, -71.8628, 42.0188, -71.1206],
        E.0,
        E.1,
    ),
    s(
        "SC",
        "South Carolina",
        "45",
        [32.0346, -83.3532, 35.2025, -78.5411],
        E.0,
        E.1,
    ),
    s(
        "SD",
        "South Dakota",
        "46",
        [42.4827, -104.0577, 45.9453, -96.4393],
        CM.0,
        CM.1,
    ),
    s(
        "TN",
        "Tennessee",
        "47",
        [34.9830, -90.3093, 36.6781, -81.6469],
        EC.0,
        EC.1,
    ),
    s(
        "TX",
        "Texas",
        "48",
        [25.8401, -106.6359, 36.5004, -93.5309],
        CM.0,
        CM.1,
    ),
    s(
        "UT",
        "Utah",
        "49",
        [36.9980, -114.0527, 42.0016, -109.0418],
        M.0,
        M.1,
    ),
    s(
        "VA",
        "Virginia",
        "51",
        [36.5407, -83.6754, 39.4660, -75.2423],
        E.0,
        E.1,
    ),
    s(
        "VT",
        "Vermont",
        "50",
        [42.7269, -73.4369, 45.0148, -71.4944],
        E.0,
        E.1,
    ),
    s(
        "WA",
        "Washington",
        "53",
        [45.5443, -124.7258, 49.0025, -116.9160],
        P.0,
        P.1,
    ),
    s(
        "WI",
        "Wisconsin",
        "55",
        [42.4919, -92.8879, 47.0547, -86.8059],
        C.0,
        C.1,
    ),
    s(
        "WV",
        "West Virginia",
        "54",
        [37.2025, -82.6262, 40.6388, -77.7195],
        E.0,
        E.1,
    ),
    s(
        "WY",
        "Wyoming",
        "56",
        [40.9963, -111.0569, 45.0059, -104.0523],
        M.0,
        M.1,
    ),
];

/// Look up a state by USPS abbreviation, full name (case-insensitive), or FIPS code.
pub fn find_state(q: &str) -> Option<&'static StateInfo> {
    let t = q.trim().trim_end_matches('.');
    if t.is_empty() {
        return None;
    }
    let norm = normalize_name(t);
    STATES.iter().find(|s| {
        s.abbr.eq_ignore_ascii_case(t)
            || normalize_name(s.name) == norm
            || s.fips == t
            || (norm == "washington dc" && s.abbr == "DC")
            || (norm == "washington state" && s.abbr == "WA")
    })
}

/// States whose bounding box contains the point (several near borders; boxes overlap).
pub fn states_containing(lat: f64, lon: f64) -> Vec<&'static StateInfo> {
    STATES
        .iter()
        .filter(|s| s.bbox().contains(lat, lon))
        .collect()
}

/// Union of UTC-offset ranges for a set of states; CONUS range when empty.
pub fn offset_union<'a, I: IntoIterator<Item = &'a StateInfo>>(it: I) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for s in it {
        lo = lo.min(s.utc_offset_h.0);
        hi = hi.max(s.utc_offset_h.1);
    }
    if lo.is_finite() {
        (lo, hi)
    } else {
        CONUS_UTC_OFFSET_H
    }
}

/// UTC-offset range implied by longitude alone (nautical zones widened by one hour each way,
/// DST included). Used when a point is outside every state box (offshore).
pub fn offset_from_longitude(lon: f64) -> (f64, f64) {
    let z = (lon / 15.0).round();
    (z - 1.0, z + 1.0 + 1.0)
}

/// Lowercase, collapse whitespace, drop periods (for "St. Louis" vs "st louis").
pub(crate) fn normalize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        if ch == '.' {
            continue;
        }
        let c = ch.to_ascii_lowercase();
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_forms() {
        assert_eq!(find_state("ny").unwrap().name, "New York");
        assert_eq!(find_state("New  york").unwrap().abbr, "NY");
        assert_eq!(find_state("36").unwrap().abbr, "NY");
        assert_eq!(find_state("Washington DC").unwrap().abbr, "DC");
        assert_eq!(find_state("Washington").unwrap().abbr, "WA");
        assert!(find_state("Atlantis").is_none());
        assert_eq!(STATES.len(), 52);
    }

    #[test]
    fn offsets() {
        assert_eq!(find_state("TX").unwrap().utc_offset_h, (-7.0, -5.0));
        assert_eq!(find_state("AZ").unwrap().utc_offset_h, (-7.0, -6.0));
        assert_eq!(find_state("HI").unwrap().utc_offset_h, (-10.0, -10.0));
        assert_eq!(find_state("AK").unwrap().utc_offset_h, (-10.0, -8.0));
        let u = offset_union([find_state("NY").unwrap(), find_state("CA").unwrap()]);
        assert_eq!(u, (-8.0, -4.0));
    }

    #[test]
    fn containing() {
        let v = states_containing(40.876466, -73.524951);
        assert!(v.iter().any(|s| s.abbr == "NY"));
    }
}
