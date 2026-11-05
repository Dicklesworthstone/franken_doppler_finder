//! Live validation: fetch New York State (padded 30 km) for 2026-09-08T04Z .. 2026-09-10T11Z
//! from adsb.lol (prod + mlatonly), cache per UTC day, and report.
//! Cache root: fdop_types::default_cache_dir() ($FDOP_CACHE, else $FDOP_CACHE_DIR, else ~/.cache/fdop).
//! Optional args: [check_key_prefix ...] to print legs whose ICAO matches.
use fdop_data::{FetchOptions, IngestSpec, fetch_window, geoid_height_or_default};
use fdop_types::BBox;
use fdop_types::time::{iso_utc, parse_iso_utc};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let cache = fdop_types::default_cache_dir();
    let bbox = BBox::new(40.5024, -79.7621, 45.0147, -71.8562).padded_km(30.0);
    println!("bbox (NY padded 30 km): {:?}", bbox);
    let (lat_c, lon_c) = bbox.center();
    let (n, src) = geoid_height_or_default(&cache, lat_c, lon_c);
    println!("geoid N at centre ({lat_c:.4},{lon_c:.4}) = {n:.3} m ({src})");
    let spec = IngestSpec {
        bbox,
        t_start: parse_iso_utc("2026-09-08T04:00:00Z").unwrap(),
        t_end: parse_iso_utc("2026-09-10T11:00:00Z").unwrap(),
        geoid_n_m: n,
        keep_unknown: true,
    };
    let opts = FetchOptions {
        progress: Some(Arc::new(|s: &str| eprintln!("{s}"))),
        ..Default::default()
    };
    let t = Instant::now();
    let store = match fetch_window(&spec, &cache, &opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fetch_window failed: {e}");
            std::process::exit(1);
        }
    };
    let dt = t.elapsed().as_secs_f64();
    println!(
        "wall {dt:.1}s: {} legs, {} points",
        store.legs.len(),
        store.n_points()
    );
    println!(
        "meta.stats = {}",
        serde_json::to_string_pretty(&store.meta.stats).unwrap()
    );
    let checks: Vec<String> = std::env::args()
        .skip(1)
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for l in &store.legs {
        if checks.iter().any(|c| l.icao.starts_with(c.as_str())) {
            println!(
                "leg {} reg={} typ={} engine={} source={} n={} {} .. {}",
                l.key,
                l.reg,
                l.typ,
                l.engine.as_str(),
                l.source,
                l.points.len(),
                iso_utc(l.t0()),
                iso_utc(l.t1())
            );
        }
    }
}
