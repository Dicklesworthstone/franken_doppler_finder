//! Report model and renderers: Markdown (frankenpandas tables), CSV, and a self-contained
//! HTML page with a Leaflet map (USGS National Map basemaps, OSM fallback).

use crate::geocode::PlaceReport;
use crate::{GeoError, esc};
use frankenpandas::{
    CsvWriteOptions, DataFrame, MarkdownWriteOptions, NullKind, Scalar,
    write_csv_string_with_options, write_markdown_string_with_options,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    /// How the radius was derived (e.g. "score-surface 2-sigma region + model floor").
    #[serde(default)]
    pub radius_basis: String,
    /// "high" / "medium" / "low".
    #[serde(default)]
    pub confidence: String,
    /// One-line verdict, e.g. "cross-confirmed by 3 events".
    #[serde(default)]
    pub verdict: String,
    /// (gate name, passed, detail)
    #[serde(default)]
    pub gates: Vec<(String, bool, String)>,
}

/// One event explained by one ADS-B flight at a candidate place and clock.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MatchRow {
    pub event: String,
    /// "pair" (drove the search) or "confirmer" (explained afterwards at the locked clock).
    pub role: String,
    /// Track key (`icao_date_leg`).
    pub flight: String,
    #[serde(default)]
    pub reg: String,
    #[serde(default)]
    pub typ: String,
    /// Trace misfit (1 = as good as the event's own straight-line fit).
    pub chi: f64,
    /// Chance-match surprise (nats).
    pub nats: f64,
    pub d_m: f64,
    pub speed_mps: f64,
    pub alt_m: f64,
    /// Clock offset this event alone prefers relative to the joint clock (s).
    #[serde(default)]
    pub dt_free_s: Option<f64>,
    pub cpa_heard_utc: String,
    /// Path-mean sound speed and wind along the flight used for this match (m/s).
    pub c_path: f64,
    pub wind_along: f64,
    pub f0: f64,
    /// Tone after a mid-pass change (prop RPM / power), if the fit found one.
    #[serde(default)]
    pub f0_after: Option<f64>,
}

impl MatchRow {
    /// "N22NY PC12" (registration if known, else the track key; plus the type).
    pub fn aircraft(&self) -> String {
        let id = if self.reg.is_empty() {
            &self.flight
        } else {
            &self.reg
        };
        if self.typ.is_empty() {
            id.clone()
        } else {
            format!("{id} {}", self.typ)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidateRow {
    pub rank: usize,
    pub lat: f64,
    pub lon: f64,
    /// Distance from the rank-1 candidate (km).
    pub km_from_top: f64,
    /// Evidence (nats): sum of the matches' chance-match surprise minus unheard-pass penalties.
    pub evidence_nats: f64,
    pub n_explained: usize,
    /// Tonal aircraft that passed much closer while recording and left no detection.
    #[serde(default)]
    pub unheard: usize,
    pub recording_start_utc: String,
    pub matches: Vec<MatchRow>,
}

/// Per-event chance-match calibration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CalibRow {
    pub event: String,
    /// (flight, place, clock) hypotheses passing the event's gates over the region.
    pub hypotheses: usize,
    pub gate_nats: f64,
    pub chi_null_p05: f64,
    pub chi_null_median: f64,
    pub n_null: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub id: String,
    /// Recording-clock time of the delayed closest approach (s).
    pub t_rec: f64,
    pub f0: f64,
    pub v: f64,
    pub d: f64,
    pub rmse: f64,
    #[serde(default)]
    pub quality: String,
    /// What the answer explains this event with (empty if nothing).
    #[serde(default)]
    pub matched: String,
    #[serde(default)]
    pub figure: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Heatmap {
    pub lats: Vec<f64>,
    pub lons: Vec<f64>,
    /// scores[i][j] at (lats[i], lons[j]); NaN / null = not scored.
    pub scores: Vec<Vec<f64>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportInput {
    pub tool: String,
    pub version: String,
    pub created_utc: String,
    pub input_file: String,
    /// Human description of the date range, e.g. "2026-09-08".
    pub dates: String,
    /// Region label as parsed, e.g. "New York".
    pub region: String,
    pub answer: Answer,
    #[serde(default)]
    pub place: Option<PlaceReport>,
    #[serde(default)]
    pub candidates: Vec<CandidateRow>,
    #[serde(default)]
    pub events: Vec<EventRow>,
    #[serde(default)]
    pub calibration: Vec<CalibRow>,
    /// Atmosphere model used (source and path-mean values).
    #[serde(default)]
    pub atmosphere: String,
    #[serde(default)]
    pub funnel: serde_json::Value,
    #[serde(default)]
    pub margin: serde_json::Value,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub heatmap: Option<Heatmap>,
    /// (caption, path relative to the report directory)
    #[serde(default)]
    pub figures: Vec<(String, String)>,
    #[serde(default)]
    pub timings: serde_json::Value,
    #[serde(default)]
    pub extra: serde_json::Value,
}

fn f(v: f64, prec: usize) -> String {
    if v.is_finite() {
        format!("{v:.prec$}")
    } else {
        "–".into()
    }
}

fn fo(v: Option<f64>, prec: usize) -> String {
    v.map_or_else(|| "–".into(), |x| f(x, prec))
}

fn num(v: f64) -> Scalar {
    if v.is_finite() {
        Scalar::Float64(v)
    } else {
        Scalar::Null(NullKind::NaN)
    }
}

fn numo(v: Option<f64>) -> Scalar {
    v.map_or(Scalar::Null(NullKind::Null), num)
}

fn txt(s: &str) -> Scalar {
    Scalar::Utf8(s.to_string())
}

fn frame(cols: Vec<(&str, Vec<Scalar>)>) -> Result<DataFrame, GeoError> {
    let order: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
    DataFrame::from_dict(&order, cols)
        .map_err(|e| GeoError::Data(format!("frankenpandas frame: {e}")))
}

/// Candidate table with display formatting (all text cells) for Markdown/HTML.
pub fn candidates_display_frame(inp: &ReportInput) -> Result<DataFrame, GeoError> {
    let c = &inp.candidates;
    let col =
        |g: &dyn Fn(&CandidateRow) -> String| c.iter().map(|r| txt(&g(r))).collect::<Vec<_>>();
    frame(vec![
        ("rank", col(&|r| r.rank.to_string())),
        ("lat", col(&|r| f(r.lat, 6))),
        ("lon", col(&|r| f(r.lon, 6))),
        ("km from #1", col(&|r| f(r.km_from_top, 1))),
        ("evidence (nats)", col(&|r| f(r.evidence_nats, 1))),
        ("events", col(&|r| r.n_explained.to_string())),
        (
            "flights (event aircraft chi)",
            col(&|r| flights_summary(&r.matches)),
        ),
        ("unheard", col(&|r| r.unheard.to_string())),
        (
            "recording start (UTC)",
            col(&|r| r.recording_start_utc.clone()),
        ),
    ])
}

/// "e15 N22NY PC12 1.16; e16 N93CQ B350 11.9 (confirmer)".
pub fn flights_summary(m: &[MatchRow]) -> String {
    m.iter()
        .map(|x| {
            format!(
                "{} {} {}{}",
                x.event,
                x.aircraft(),
                f(x.chi, 2),
                if x.role == "confirmer" {
                    " (confirmer)"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The winning candidate's matches, one row per event.
pub fn matches_display_frame(inp: &ReportInput) -> Result<DataFrame, GeoError> {
    let m: &[MatchRow] = inp.candidates.first().map_or(&[], |c| &c.matches);
    let col = |g: &dyn Fn(&MatchRow) -> String| m.iter().map(|r| txt(&g(r))).collect::<Vec<_>>();
    frame(vec![
        ("event", col(&|r| r.event.clone())),
        ("role", col(&|r| r.role.clone())),
        ("aircraft", col(&|r| r.aircraft())),
        ("track", col(&|r| r.flight.clone())),
        ("chi", col(&|r| f(r.chi, 2))),
        ("nats", col(&|r| f(r.nats, 1))),
        ("CPA dist (m)", col(&|r| f(r.d_m, 0))),
        ("speed (m/s)", col(&|r| f(r.speed_mps, 1))),
        ("alt MSL (m)", col(&|r| f(r.alt_m, 0))),
        ("own clock (s)", col(&|r| fo(r.dt_free_s, 1))),
        ("CPA heard (UTC)", col(&|r| r.cpa_heard_utc.clone())),
        ("c path (m/s)", col(&|r| f(r.c_path, 1))),
        ("wind along (m/s)", col(&|r| f(r.wind_along, 1))),
        (
            "f0 (Hz)",
            col(&|r| match r.f0_after {
                Some(b) => format!("{} -> {}", f(r.f0, 1), f(b, 1)),
                None => f(r.f0, 2),
            }),
        ),
    ])
}

pub fn calibration_display_frame(inp: &ReportInput) -> Result<DataFrame, GeoError> {
    let c = &inp.calibration;
    let col = |g: &dyn Fn(&CalibRow) -> String| c.iter().map(|r| txt(&g(r))).collect::<Vec<_>>();
    frame(vec![
        ("event", col(&|r| r.event.clone())),
        ("hypotheses", col(&|r| r.hypotheses.to_string())),
        ("gate (nats)", col(&|r| f(r.gate_nats, 1))),
        ("null chi p05", col(&|r| f(r.chi_null_p05, 2))),
        ("null chi median", col(&|r| f(r.chi_null_median, 2))),
        ("null samples", col(&|r| r.n_null.to_string())),
    ])
}

/// Numeric candidate table (CSV / machine use): one row per (candidate, matched event).
pub fn candidates_frame(inp: &ReportInput) -> Result<DataFrame, GeoError> {
    let rows: Vec<(&CandidateRow, &MatchRow)> = inp
        .candidates
        .iter()
        .flat_map(|c| c.matches.iter().map(move |m| (c, m)))
        .collect();
    let n = |g: &dyn Fn(&CandidateRow, &MatchRow) -> Scalar| {
        rows.iter().map(|(c, m)| g(c, m)).collect::<Vec<_>>()
    };
    frame(vec![
        ("rank", n(&|c, _| Scalar::Int64(c.rank as i64))),
        ("lat", n(&|c, _| num(c.lat))),
        ("lon", n(&|c, _| num(c.lon))),
        ("km_from_top", n(&|c, _| num(c.km_from_top))),
        ("evidence_nats", n(&|c, _| num(c.evidence_nats))),
        (
            "n_explained",
            n(&|c, _| Scalar::Int64(c.n_explained as i64)),
        ),
        ("unheard", n(&|c, _| Scalar::Int64(c.unheard as i64))),
        (
            "recording_start_utc",
            n(&|c, _| txt(&c.recording_start_utc)),
        ),
        ("event", n(&|_, m| txt(&m.event))),
        ("role", n(&|_, m| txt(&m.role))),
        ("flight", n(&|_, m| txt(&m.flight))),
        ("reg", n(&|_, m| txt(&m.reg))),
        ("type", n(&|_, m| txt(&m.typ))),
        ("chi", n(&|_, m| num(m.chi))),
        ("nats", n(&|_, m| num(m.nats))),
        ("d_m", n(&|_, m| num(m.d_m))),
        ("speed_mps", n(&|_, m| num(m.speed_mps))),
        ("alt_m", n(&|_, m| num(m.alt_m))),
        ("dt_free_s", n(&|_, m| numo(m.dt_free_s))),
        ("cpa_heard_utc", n(&|_, m| txt(&m.cpa_heard_utc))),
        ("c_path", n(&|_, m| num(m.c_path))),
        ("wind_along", n(&|_, m| num(m.wind_along))),
        ("f0", n(&|_, m| num(m.f0))),
        ("f0_after", n(&|_, m| numo(m.f0_after))),
    ])
}

const CANDIDATES_CSV_HEADER: &str = "rank,lat,lon,km_from_top,evidence_nats,n_explained,unheard,recording_start_utc,event,role,flight,reg,type,chi,nats,d_m,speed_mps,alt_m,dt_free_s,cpa_heard_utc,c_path,wind_along,f0,f0_after\n";

pub fn events_display_frame(inp: &ReportInput) -> Result<DataFrame, GeoError> {
    let e = &inp.events;
    let col = |g: &dyn Fn(&EventRow) -> String| e.iter().map(|r| txt(&g(r))).collect::<Vec<_>>();
    frame(vec![
        ("event", col(&|r| r.id.clone())),
        ("t_rec (s)", col(&|r| f(r.t_rec, 2))),
        ("f0 (Hz)", col(&|r| f(r.f0, 2))),
        ("v (m/s)", col(&|r| f(r.v, 1))),
        ("d (m)", col(&|r| f(r.d, 0))),
        ("rmse (Hz)", col(&|r| f(r.rmse, 2))),
        ("quality", col(&|r| r.quality.clone())),
        (
            "explained by",
            col(&|r| {
                if r.matched.is_empty() {
                    "–".into()
                } else {
                    r.matched.clone()
                }
            }),
        ),
    ])
}

fn md_table(df: &DataFrame) -> Result<String, GeoError> {
    let opts = MarkdownWriteOptions {
        include_index: false,
        na_rep: "–".into(),
        index_label: None,
    };
    write_markdown_string_with_options(df, &opts)
        .map_err(|e| GeoError::Data(format!("markdown table: {e}")))
}

/// Write the numeric candidate table as CSV (frankenpandas writer).
pub fn write_candidates_csv(inp: &ReportInput, path: &Path) -> Result<(), GeoError> {
    let df = candidates_frame(inp)?;
    let opts = CsvWriteOptions {
        include_index: false,
        ..CsvWriteOptions::default()
    };
    let s = if inp.candidates.iter().all(|c| c.matches.is_empty()) {
        CANDIDATES_CSV_HEADER.to_string()
    } else {
        write_csv_string_with_options(&df, &opts)
            .map_err(|e| GeoError::Data(format!("csv: {e}")))?
    };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, s)?;
    Ok(())
}

/// Markdown report.
pub fn render_markdown(inp: &ReportInput) -> String {
    let a = &inp.answer;
    let mut o = String::new();
    o.push_str(&format!(
        "# Doppler geolocation report: {}\n\n",
        if inp.input_file.is_empty() {
            "recording"
        } else {
            &inp.input_file
        }
    ));
    if let Some(p) = &inp.place {
        o.push_str(&format!("**{}**\n\n", p.headline));
    }
    o.push_str(&format!(
        "- **Location:** {:.6}, {:.6} (±{:.0} m)\n- **Confidence:** {}\n- **Verdict:** {}\n",
        a.lat,
        a.lon,
        a.radius_m,
        if a.confidence.is_empty() {
            "–"
        } else {
            &a.confidence
        },
        if a.verdict.is_empty() {
            "–"
        } else {
            &a.verdict
        }
    ));
    if !a.radius_basis.is_empty() {
        o.push_str(&format!("- **Radius basis:** {}\n", a.radius_basis));
    }
    o.push_str(&format!(
        "- **Search:** region {}; dates {}\n- **Generated:** {} by {} {}\n",
        inp.region, inp.dates, inp.created_utc, inp.tool, inp.version
    ));
    o.push_str(&format!(
        "- **Maps:** [OpenStreetMap](https://www.openstreetmap.org/?mlat={lat:.6}&mlon={lon:.6}#map=16/{lat:.6}/{lon:.6}) · \
         [USGS National Map](https://apps.nationalmap.gov/viewer/?center={lon:.6},{lat:.6}&level=15)\n\n",
        lat = a.lat,
        lon = a.lon
    ));
    if let Some(p) = &inp.place {
        o.push_str("## Place\n\n");
        let row = |k: &str, v: &Option<String>| {
            v.as_ref()
                .map(|x| format!("- {k}: {x}\n"))
                .unwrap_or_default()
        };
        o.push_str(&row(
            "Place",
            &p.place
                .as_ref()
                .map(|x| format!("{x} ({})", p.place_kind.clone().unwrap_or_default())),
        ));
        o.push_str(&row("Town / county subdivision", &p.county_subdivision));
        o.push_str(&row("County", &p.county));
        o.push_str(&row("State", &p.state));
        if let Some(z) = &p.zip {
            o.push_str(&format!("- ZIP (ZCTA): {z} [{}]\n", p.zip_source));
        }
        o.push_str(&format!("- Source: {}\n", p.source));
        if !p.nearest_places.is_empty() {
            o.push_str("- Nearest places: ");
            let v: Vec<String> = p
                .nearest_places
                .iter()
                .map(|n| {
                    format!(
                        "{} {} (answer {:.1} km {} of its centre)",
                        n.name,
                        n.kind,
                        n.distance_m / 1000.0,
                        n.compass
                    )
                })
                .collect();
            o.push_str(&v.join("; "));
            o.push('\n');
        }
        for n in &p.notes {
            o.push_str(&format!("- Note: {n}\n"));
        }
        o.push('\n');
    }
    if !a.gates.is_empty() {
        o.push_str("## Gates\n\n| gate | result | detail |\n|---|---|---|\n");
        for (g, ok, d) in &a.gates {
            o.push_str(&format!(
                "| {g} | {} | {} |\n",
                if *ok { "pass" } else { "FAIL" },
                d.replace('|', "\\|")
            ));
        }
        o.push('\n');
    }
    let mut section = |title: &str, note: &str, empty: bool, df: Result<DataFrame, GeoError>| {
        o.push_str(&format!("## {title}\n\n"));
        if !note.is_empty() {
            o.push_str(note);
            o.push_str("\n\n");
        }
        if empty {
            o.push_str("_none_\n\n");
            return;
        }
        match df.and_then(|d| md_table(&d)) {
            Ok(t) => {
                o.push_str(&t);
                o.push_str("\n\n");
            }
            Err(e) => o.push_str(&format!("_table unavailable: {e}_\n\n")),
        }
    };
    let no_matches = inp.candidates.first().is_none_or(|c| c.matches.is_empty());
    section(
        "Winning matches",
        MATCHES_NOTE,
        no_matches,
        matches_display_frame(inp),
    );
    section(
        "Ranked candidates",
        "",
        inp.candidates.is_empty(),
        candidates_display_frame(inp),
    );
    section(
        "Events",
        "",
        inp.events.is_empty(),
        events_display_frame(inp),
    );
    section(
        "Chance-match calibration",
        CALIB_NOTE,
        inp.calibration.is_empty(),
        calibration_display_frame(inp),
    );
    if !inp.atmosphere.is_empty() {
        o.push_str(&format!("## Atmosphere\n\n{}\n\n", inp.atmosphere));
    }
    for (label, v) in [
        ("Funnel", &inp.funnel),
        ("Margin", &inp.margin),
        ("Timings", &inp.timings),
    ] {
        if !v.is_null() {
            o.push_str(&format!(
                "## {label}\n\n```json\n{}\n```\n\n",
                serde_json::to_string_pretty(v).unwrap_or_default()
            ));
        }
    }
    if !inp.warnings.is_empty() {
        o.push_str("## Warnings\n\n");
        for w in &inp.warnings {
            o.push_str(&format!("- {w}\n"));
        }
        o.push('\n');
    }
    if !inp.figures.is_empty() {
        o.push_str("## Figures\n\n");
        for (cap, path) in &inp.figures {
            o.push_str(&format!("![{cap}]({path})\n\n*{cap}*\n\n"));
        }
    }
    o.push_str(ATTRIBUTION_MD);
    o
}

const MATCHES_NOTE: &str = "chi = trace misfit (1 = as good as the event's own straight-line fit); nats = how surprising a \
match this good would be by chance; own clock = the recording-clock shift this event alone would prefer (near 0 = locked).";

const CALIB_NOTE: &str = "How often a random flight, place and clock passes each event's gates (gate nats) and how well such \
chance matches fit the trace (null chi). A match scores more nats the further its chi is below the null.";

const ATTRIBUTION_MD: &str = "---\nData: aircraft tracks © adsb.lol contributors (ODbL 1.0); basemaps USGS The National Map \
(public domain) and © OpenStreetMap contributors; places and ZCTAs US Census Bureau (Gazetteer, Geocoder); terrain AWS Terrain \
Tiles (Mapzen terrarium). The location is a cluster centre with an uncertainty radius, never a street address.\n";

const LEAFLET_CSS: &str = "https://unpkg.com/leaflet@1.9.4/dist/leaflet.css";
const LEAFLET_CSS_SRI: &str = "sha256-p4NxAoJBhIIN+hmNHrzRCf9tD/miZyoHS5obTRR9BMY=";
const LEAFLET_JS: &str = "https://unpkg.com/leaflet@1.9.4/dist/leaflet.js";
const LEAFLET_JS_SRI: &str = "sha256-20nQCchB9co0qIjJZRGuk2/Z9VM+kNiyxNV1lvTlZBo=";

fn html_table(df: Result<DataFrame, GeoError>) -> String {
    match df.and_then(|d| md_table(&d)) {
        Ok(md) => md_to_html_table(&md),
        Err(e) => format!("<p><em>table unavailable: {}</em></p>", esc(&e.to_string())),
    }
}

/// Convert a pipe Markdown table (as written by frankenpandas) into an HTML table.
fn md_to_html_table(md: &str) -> String {
    let mut rows = md.lines().filter(|l| l.trim_start().starts_with('|'));
    let cells = |l: &str| -> Vec<String> {
        let t = l.trim().trim_start_matches('|').trim_end_matches('|');
        t.split('|').map(|c| esc(c.trim())).collect()
    };
    let mut o = String::from("<table>\n<thead><tr>");
    if let Some(h) = rows.next() {
        for c in cells(h) {
            o.push_str(&format!("<th>{c}</th>"));
        }
    }
    o.push_str("</tr></thead>\n<tbody>\n");
    for l in rows {
        if l.contains("---") && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            continue;
        }
        o.push_str("<tr>");
        for c in cells(l) {
            o.push_str(&format!("<td>{c}</td>"));
        }
        o.push_str("</tr>\n");
    }
    o.push_str("</tbody></table>\n");
    o
}

/// Write `report.html` (Leaflet map + tables + figures) into `out_dir`; returns its path.
pub fn render_html(inp: &ReportInput, out_dir: &Path) -> Result<PathBuf, GeoError> {
    std::fs::create_dir_all(out_dir)?;
    let a = &inp.answer;
    let title = inp.place.as_ref().map_or_else(
        || format!("{:.5}, {:.5}", a.lat, a.lon),
        |p| p.headline.clone(),
    );
    let cands: Vec<serde_json::Value> = inp
        .candidates
        .iter()
        .take(20)
        .map(|c| {
            serde_json::json!({"rank": c.rank, "lat": c.lat, "lon": c.lon, "flights": flights_summary(&c.matches),
                               "evidence": if c.evidence_nats.is_finite() { c.evidence_nats } else { 0.0 }})
        })
        .collect();
    let heat = inp.heatmap.as_ref().map(heat_cells).unwrap_or_default();
    let data = serde_json::json!({
        "lat": a.lat, "lon": a.lon, "radius": a.radius_m, "title": title,
        "candidates": cands, "heat": heat,
    });
    let mut gates = String::new();
    if !a.gates.is_empty() {
        gates.push_str("<h2>Gates</h2><table><thead><tr><th>gate</th><th>result</th><th>detail</th></tr></thead><tbody>");
        for (g, ok, d) in &a.gates {
            gates.push_str(&format!(
                "<tr><td>{}</td><td class=\"{}\">{}</td><td>{}</td></tr>",
                esc(g),
                if *ok { "pass" } else { "fail" },
                if *ok { "pass" } else { "FAIL" },
                esc(d)
            ));
        }
        gates.push_str("</tbody></table>");
    }
    let mut place = String::new();
    if let Some(p) = &inp.place {
        place.push_str("<h2>Place</h2><ul>");
        let mut li = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                place.push_str(&format!("<li><b>{}</b>: {}</li>", esc(k), esc(&v)));
            }
        };
        li(
            "Place",
            p.place
                .as_ref()
                .map(|x| format!("{x} ({})", p.place_kind.clone().unwrap_or_default())),
        );
        li("Town / county subdivision", p.county_subdivision.clone());
        li("County", p.county.clone());
        li("State", p.state.clone());
        li(
            "ZIP (ZCTA)",
            p.zip.as_ref().map(|z| format!("{z} [{}]", p.zip_source)),
        );
        li("Source", Some(p.source.clone()));
        if !p.nearest_places.is_empty() {
            li(
                "Nearest places",
                Some(
                    p.nearest_places
                        .iter()
                        .map(|n| {
                            format!(
                                "{} {} (answer {:.1} km {} of its centre)",
                                n.name,
                                n.kind,
                                n.distance_m / 1000.0,
                                n.compass
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            );
        }
        for n in &p.notes {
            li("Note", Some(n.clone()));
        }
        place.push_str("</ul>");
    }
    let mut figs = String::new();
    if !inp.figures.is_empty() {
        figs.push_str("<h2>Figures</h2>");
        for (cap, path) in &inp.figures {
            figs.push_str(&format!(
                "<figure><a href=\"{p}\"><img src=\"{p}\" alt=\"{c}\" loading=\"lazy\"></a><figcaption>{c}</figcaption></figure>",
                p = esc(path),
                c = esc(cap)
            ));
        }
    }
    let mut warn = String::new();
    if !inp.warnings.is_empty() {
        warn.push_str("<h2>Warnings</h2><ul class=\"warn\">");
        for w in &inp.warnings {
            warn.push_str(&format!("<li>{}</li>", esc(w)));
        }
        warn.push_str("</ul>");
    }
    let mut jsonblocks = String::new();
    for (label, v) in [
        ("Funnel", &inp.funnel),
        ("Margin", &inp.margin),
        ("Timings", &inp.timings),
    ] {
        if !v.is_null() {
            jsonblocks.push_str(&format!(
                "<details><summary>{label}</summary><pre>{}</pre></details>",
                esc(&serde_json::to_string_pretty(v).unwrap_or_default())
            ));
        }
    }
    let cand_html = if inp.candidates.is_empty() {
        "<p><em>none</em></p>".to_string()
    } else {
        html_table(candidates_display_frame(inp))
    };
    let ev_html = if inp.events.is_empty() {
        "<p><em>none</em></p>".to_string()
    } else {
        html_table(events_display_frame(inp))
    };
    let match_html = if inp.candidates.first().is_none_or(|c| c.matches.is_empty()) {
        "<p><em>none</em></p>".to_string()
    } else {
        format!(
            "<p class=\"note\">{}</p>{}",
            esc(MATCHES_NOTE),
            html_table(matches_display_frame(inp))
        )
    };
    let calib_html = if inp.calibration.is_empty() {
        String::new()
    } else {
        format!(
            "<h2>Chance-match calibration</h2><p class=\"note\">{}</p><div class=\"scroll\">{}</div>",
            esc(CALIB_NOTE),
            html_table(calibration_display_frame(inp))
        )
    };
    let atmo_html = if inp.atmosphere.is_empty() {
        String::new()
    } else {
        format!("<h2>Atmosphere</h2><p>{}</p>", esc(&inp.atmosphere))
    };
    // `</` inside a <script> JSON literal must not close the script element.
    let data_js = serde_json::to_string(&data)
        .unwrap_or_else(|_| "{}".into())
        .replace("</", "<\\/");
    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title_e} — Doppler geolocation</title>
<link rel="stylesheet" href="{css}" integrity="{css_sri}" crossorigin="">
<script src="{js}" integrity="{js_sri}" crossorigin=""></script>
<style>
 body {{ font: 15px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; margin: 0; color: #1b1f24; }}
 header {{ padding: 14px 20px; background: #0f2a3d; color: #fff; }}
 header h1 {{ margin: 0 0 4px; font-size: 21px; }}
 header .sub {{ opacity: .85; font-size: 14px; }}
 main {{ padding: 0 20px 30px; max-width: 1400px; }}
 #map {{ height: 560px; margin: 16px 0; border: 1px solid #c8d0d8; border-radius: 6px; }}
 table {{ border-collapse: collapse; font-size: 13px; margin: 8px 0 18px; }}
 th, td {{ border: 1px solid #d5dbe1; padding: 4px 7px; text-align: left; white-space: nowrap; }}
 th {{ background: #eef2f6; }}
 td.pass {{ color: #11723a; font-weight: 600; }} td.fail {{ color: #b3261e; font-weight: 700; }}
 .kv b {{ display: inline-block; min-width: 110px; }}
 figure {{ margin: 12px 0; }} figure img {{ max-width: 100%; border: 1px solid #d5dbe1; }}
 figcaption {{ font-size: 13px; color: #555; }}
 .warn li {{ color: #8a4b00; }}
 .scroll {{ overflow-x: auto; }}
 .note {{ font-size: 13px; color: #555; max-width: 1000px; }}
 footer {{ font-size: 12px; color: #666; padding: 12px 20px; border-top: 1px solid #e3e7eb; }}
</style></head>
<body>
<header><h1>{title_e}</h1>
<div class="sub">{lat:.6}, {lon:.6} &nbsp;±{rad:.0} m &nbsp;·&nbsp; confidence: {conf} &nbsp;·&nbsp; {verdict}</div></header>
<main>
<div id="map"></div>
<div class="kv"><p><b>Recording</b> {file}<br><b>Dates</b> {dates}<br><b>Region</b> {region}<br><b>Radius basis</b> {basis}<br><b>Generated</b> {created} by {tool} {version}</p></div>
{gates}
{place}
<h2>Winning matches</h2><div class="scroll">{match_html}</div>
<h2>Ranked candidates</h2><div class="scroll">{cand_html}</div>
<h2>Events</h2><div class="scroll">{ev_html}</div>
{calib_html}
{atmo_html}
{warn}
{figs}
{jsonblocks}
</main>
<footer>Aircraft tracks © adsb.lol contributors (ODbL 1.0) · Basemaps: USGS The National Map (public domain), © OpenStreetMap contributors · Places / ZCTAs: US Census Bureau · Terrain: AWS Terrain Tiles.
The answer is a cluster centre with an uncertainty radius, not a street address.</footer>
<script>
const D = {data_js};
const topo = L.tileLayer('https://basemap.nationalmap.gov/arcgis/rest/services/USGSTopo/MapServer/tile/{{z}}/{{y}}/{{x}}',
  {{maxZoom: 19, maxNativeZoom: 16, attribution: 'USGS The National Map'}});
const imagery = L.tileLayer('https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer/tile/{{z}}/{{y}}/{{x}}',
  {{maxZoom: 19, maxNativeZoom: 16, attribution: 'USGS The National Map'}});
const osm = L.tileLayer('https://tile.openstreetmap.org/{{z}}/{{x}}/{{y}}.png',
  {{maxZoom: 19, attribution: '&copy; OpenStreetMap contributors'}});
const map = L.map('map', {{layers: [topo]}});
const overlays = {{}};
if (D.heat && D.heat.length) {{
  const g = L.layerGroup();
  for (const c of D.heat) {{
    L.rectangle([[c[0], c[1]], [c[2], c[3]]], {{stroke: false, fillColor: c[4], fillOpacity: 0.45, interactive: false}}).addTo(g);
  }}
  g.addTo(map); overlays['Score heat map'] = g;
}}
const cg = L.layerGroup();
for (const c of D.candidates) {{
  L.circleMarker([c.lat, c.lon], {{radius: c.rank === 1 ? 6 : 4, color: '#1f4e9e', weight: 1, fillOpacity: 0.8}})
    .bindTooltip('#' + c.rank + ' ' + c.flights + ' (evidence ' + c.evidence.toFixed(1) + ' nats)').addTo(cg);
}}
cg.addTo(map); overlays['Candidates'] = cg;
const circle = L.circle([D.lat, D.lon], {{radius: D.radius, color: '#d7191c', weight: 3, fill: false}}).addTo(map);
L.marker([D.lat, D.lon]).bindPopup('<b>' + D.title + '</b><br>' + D.lat.toFixed(6) + ', ' + D.lon.toFixed(6) + '<br>±' + Math.round(D.radius) + ' m').addTo(map);
overlays['Answer radius'] = circle;
L.control.layers({{'USGS Topo': topo, 'USGS Imagery': imagery, 'OpenStreetMap': osm}}, overlays).addTo(map);
L.control.scale({{imperial: true, metric: true}}).addTo(map);
map.fitBounds(circle.getBounds().pad(1.5));
</script>
</body></html>
"##,
        title_e = esc(&title),
        css = LEAFLET_CSS,
        css_sri = LEAFLET_CSS_SRI,
        js = LEAFLET_JS,
        js_sri = LEAFLET_JS_SRI,
        lat = a.lat,
        lon = a.lon,
        rad = a.radius_m,
        conf = esc(if a.confidence.is_empty() {
            "–"
        } else {
            &a.confidence
        }),
        verdict = esc(&a.verdict),
        file = esc(&inp.input_file),
        dates = esc(&inp.dates),
        region = esc(&inp.region),
        basis = esc(&a.radius_basis),
        created = esc(&inp.created_utc),
        tool = esc(&inp.tool),
        version = esc(&inp.version),
    );
    let path = out_dir.join("report.html");
    std::fs::write(&path, html)?;
    Ok(path)
}

/// Heat-map cells as [south, west, north, east, colour] for finite scores.
fn heat_cells(h: &Heatmap) -> Vec<serde_json::Value> {
    let finite: Vec<f64> = h
        .scores
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if finite.is_empty() || h.lats.is_empty() || h.lons.is_empty() {
        return Vec::new();
    }
    let lo = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1e-12);
    let half = |v: &[f64], i: usize| -> f64 {
        if v.len() < 2 {
            0.0001
        } else if i + 1 < v.len() {
            (v[i + 1] - v[i]).abs() / 2.0
        } else {
            (v[i] - v[i - 1]).abs() / 2.0
        }
    };
    let mut out = Vec::new();
    for (i, row) in h.scores.iter().enumerate().take(h.lats.len()) {
        for (j, &s) in row.iter().enumerate().take(h.lons.len()) {
            if !s.is_finite() {
                continue;
            }
            let t = (s - lo) / span;
            if t < 0.15 {
                continue; // keep the map readable: only the upper part of the surface
            }
            let (dl, dn) = (half(&h.lats, i), half(&h.lons, j));
            out.push(serde_json::json!([
                h.lats[i] - dl,
                h.lons[j] - dn,
                h.lats[i] + dl,
                h.lons[j] + dn,
                ramp(t)
            ]));
        }
    }
    out
}

/// Perceptual-ish blue -> yellow -> red ramp for t in [0, 1].
pub fn ramp(t: f64) -> String {
    let t = t.clamp(0.0, 1.0);
    let stops = [
        (0.0, (49, 54, 149)),
        (0.35, (69, 117, 180)),
        (0.6, (254, 224, 144)),
        (0.8, (253, 174, 97)),
        (1.0, (215, 25, 28)),
    ];
    for w in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let u = (t - t0) / (t1 - t0);
            let mix =
                |a: i32, b: i32| (f64::from(a) + (f64::from(b) - f64::from(a)) * u).round() as i32;
            return format!(
                "#{:02x}{:02x}{:02x}",
                mix(c0.0, c1.0),
                mix(c0.1, c1.1),
                mix(c0.2, c1.2)
            );
        }
    }
    "#d7191c".into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sample() -> ReportInput {
        ReportInput {
            tool: "fdop".into(),
            version: "0.1.0".into(),
            created_utc: "2026-11-03T08:00:00Z".into(),
            input_file: "ref_raw.m4a".into(),
            dates: "2026-09-08".into(),
            region: "New York".into(),
            answer: Answer {
                lat: 40.876466,
                lon: -73.524951,
                radius_m: 300.0,
                radius_basis: "score surface".into(),
                confidence: "high".into(),
                verdict: "cross-confirmed".into(),
                gates: vec![
                    ("G-lock".into(), true, "lock 0.95".into()),
                    ("G-margin".into(), false, "ratio 0.85 | > 0.8".into()),
                ],
            },
            place: Some(PlaceReport {
                headline: "Oyster Bay, NY 11771 (Nassau County)".into(),
                lat: 40.876466,
                lon: -73.524951,
                place: Some("Oyster Bay".into()),
                place_kind: Some("CDP".into()),
                zip: Some("11771".into()),
                zip_source: "census-api".into(),
                source: "census-api".into(),
                ..Default::default()
            }),
            candidates: vec![
                CandidateRow {
                    rank: 1,
                    lat: 40.8765,
                    lon: -73.525,
                    km_from_top: 0.0,
                    evidence_nats: 37.7,
                    n_explained: 2,
                    unheard: 0,
                    recording_start_utc: "2026-09-08T20:17:29Z".into(),
                    matches: vec![
                        MatchRow {
                            event: "e15".into(),
                            role: "pair".into(),
                            flight: "a1b2c3_0".into(),
                            reg: "N22NY".into(),
                            typ: "PC12".into(),
                            chi: 1.16,
                            nats: 18.7,
                            d_m: 6117.0,
                            speed_mps: 136.9,
                            alt_m: 5557.0,
                            dt_free_s: Some(0.0),
                            cpa_heard_utc: "2026-09-08T20:53:25Z".into(),
                            c_path: 337.0,
                            wind_along: 2.0,
                            f0: 112.7,
                            f0_after: None,
                        },
                        MatchRow {
                            event: "e16".into(),
                            role: "confirmer".into(),
                            flight: "d4e5f6_0".into(),
                            typ: "B350".into(),
                            chi: 11.9,
                            nats: 8.6,
                            dt_free_s: None,
                            ..Default::default()
                        },
                    ],
                },
                CandidateRow {
                    rank: 2,
                    lat: 40.9,
                    lon: -73.4,
                    km_from_top: 11.2,
                    evidence_nats: 13.0,
                    n_explained: 1,
                    recording_start_utc: "2026-09-08T20:00:00Z".into(),
                    matches: vec![MatchRow {
                        event: "e1".into(),
                        role: "pair".into(),
                        flight: "aaaaaa_0".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            calibration: vec![CalibRow {
                event: "e15".into(),
                hypotheses: 3_389_347,
                gate_nats: 6.2,
                chi_null_p05: 9.1,
                chi_null_median: 31.0,
                n_null: 400,
            }],
            atmosphere: "standard atmosphere 15 C".into(),
            events: vec![EventRow {
                id: "e1".into(),
                t_rec: 5755.94,
                f0: 112.69,
                v: 136.9,
                d: 6117.0,
                rmse: 0.4,
                quality: "good".into(),
                matched: "N22NY PC12".into(),
                figure: None,
            }],
            funnel: serde_json::json!({"offset_pairs": 10}),
            margin: serde_json::json!({"ratio_top2": 0.63}),
            warnings: vec!["test warning <b>".into()],
            heatmap: Some(Heatmap {
                lats: vec![40.876, 40.8765, 40.877],
                lons: vec![-73.5255, -73.525, -73.5245],
                scores: vec![
                    vec![1.0, 2.0, 1.0],
                    vec![2.0, 5.0, f64::NAN],
                    vec![1.0, 2.0, 1.0],
                ],
            }),
            figures: vec![("event 1".into(), "e1.png".into())],
            timings: serde_json::json!({"total_s": 12.0}),
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn markdown_has_tables() {
        let md = render_markdown(&sample());
        assert!(md.contains("Oyster Bay, NY 11771 (Nassau County)"));
        assert!(md.contains("40.876466, -73.524951"));
        assert!(md.contains("| rank |"), "{md}");
        assert!(
            md.contains("e15 N22NY PC12 1.16; e16 d4e5f6_0 B350 11.90 (confirmer)"),
            "{md}"
        );
        assert!(md.contains("## Winning matches"));
        assert!(
            md.contains("| e15 | pair | N22NY PC12 | a1b2c3_0 |"),
            "{md}"
        );
        assert!(md.contains("## Chance-match calibration"));
        assert!(md.contains("standard atmosphere 15 C"));
        assert!(md.contains("| e1 |"));
        assert!(md.contains("ratio 0.85 \\| > 0.8"));
        assert!(md.contains("![event 1](e1.png)"));
    }

    #[test]
    fn html_has_map_and_circle() {
        let dir = std::env::temp_dir().join(format!("fdop-geo-html-{}", std::process::id()));
        let p = render_html(&sample(), &dir).unwrap();
        let h = std::fs::read_to_string(p).unwrap();
        assert!(h.contains("L.circle([D.lat, D.lon]"));
        assert!(h.contains("\"lat\":40.876466"));
        assert!(h.contains("basemap.nationalmap.gov"));
        assert!(h.contains("test warning &lt;b&gt;"));
        assert!(h.contains("<th>rank</th>"));
        assert!(h.contains(LEAFLET_JS_SRI));
    }

    #[test]
    fn csv_roundtrips_header() {
        let dir = std::env::temp_dir().join(format!("fdop-geo-csv-{}", std::process::id()));
        let p = dir.join("candidates.csv");
        write_candidates_csv(&sample(), &p).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.starts_with("rank,lat,lon,km_from_top"), "{s}");
        assert_eq!(s.lines().count(), 4, "{s}");
        let empty = ReportInput::default();
        write_candidates_csv(&empty, &p).unwrap();
        let h = std::fs::read_to_string(&p).unwrap();
        let df_cols = candidates_frame(&sample()).unwrap().column_names().len();
        assert_eq!(
            h.trim_end().split(',').count(),
            df_cols,
            "empty-CSV header matches the frame"
        );
    }

    #[test]
    fn heat_cells_skip_nan_and_low() {
        let cells = heat_cells(sample().heatmap.as_ref().unwrap());
        assert!(!cells.is_empty() && cells.len() < 9);
        assert_eq!(ramp(1.0), "#d7191c");
    }
}
