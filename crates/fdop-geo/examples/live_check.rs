//! Live check against the real Census services and USGS tiles.
//! Usage: live_check <out_dir>
use fdop_geo::gazetteer::Gazetteer;
use fdop_geo::geocode::describe_location;
use fdop_geo::region::{RegionContext, parse_region};
use fdop_geo::report::{Answer, ReportInput, render_html, render_markdown, write_candidates_csv};
use fdop_geo::staticmap::{StaticMapOptions, static_map};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("out dir"));
    let cache = fdop_geo::default_cache_dir();
    let ctx = RegionContext::new(cache.clone(), true);
    for q in [
        "NY",
        "Oyster Bay, NY",
        "11771",
        "Nassau County, NY",
        "CONUS",
        "40.87,-73.52,20",
        "Springfield",
        "New York, NY",
    ] {
        let t = Instant::now();
        match parse_region(q, &ctx) {
            Ok(r) => println!(
                "{q:>20} -> {:?} '{}' bbox [{:.4}, {:.4}, {:.4}, {:.4}] r={:?} utc{:?} states={:?} ({:.2}s)",
                r.kind,
                r.label,
                r.bbox.south,
                r.bbox.west,
                r.bbox.north,
                r.bbox.east,
                r.radius_km.map(|x| (x * 10.0).round() / 10.0),
                r.utc_offset_h,
                r.states,
                t.elapsed().as_secs_f64()
            ),
            Err(e) => println!("{q:>20} -> ERROR {e}"),
        }
    }
    let gaz = Gazetteer::new(cache.clone(), true);
    let net = fdop_http::Net::new().expect("net");
    let (lat, lon) = (40.876466, -73.524951);
    let t = Instant::now();
    let rep = describe_location(Some(&net), &gaz, lat, lon);
    println!(
        "describe_location ({:.2}s): {}",
        t.elapsed().as_secs_f64(),
        serde_json::to_string_pretty(&rep).unwrap()
    );
    let off = describe_location(None, &gaz, lat, lon);
    println!("offline headline: {} | notes {:?}", off.headline, off.notes);
    let opts = StaticMapOptions {
        cache_dir: cache.clone(),
        headline: Some(rep.headline.clone()),
        markers: vec![(40.8790, -73.5300)],
        ..Default::default()
    };
    let t = Instant::now();
    let m = static_map(Some(&net), lat, lon, 300.0, &out, &opts).expect("static map");
    println!("static_map ({:.2}s): {m:?}", t.elapsed().as_secs_f64());
    let inp = ReportInput {
        tool: "fdop".into(),
        version: "0.1.0".into(),
        created_utc: "2026-11-03T08:00:00Z".into(),
        input_file: "ref_raw.m4a".into(),
        dates: "2026-09-08".into(),
        region: "New York".into(),
        answer: Answer {
            lat,
            lon,
            radius_m: 300.0,
            radius_basis: "live check".into(),
            confidence: "high".into(),
            verdict: "live check of the renderers".into(),
            gates: vec![("G-lock".into(), true, "0.95".into())],
        },
        place: Some(rep),
        figures: vec![("static map".into(), "map.png".into())],
        ..Default::default()
    };
    let p = render_html(&inp, &out).expect("html");
    std::fs::write(out.join("report.md"), render_markdown(&inp)).expect("md");
    write_candidates_csv(&inp, &out.join("candidates.csv")).expect("csv");
    println!("html: {}", p.display());
}
