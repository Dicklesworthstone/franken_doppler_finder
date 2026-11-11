//! Cache-only check of the NY validation store against reference-oracle samples.
//! Run after `ingest_ny` has populated the per-day cache.
use fdop_data::{FetchOptions, IngestSpec, fetch_window, geoid_height_or_default};
use fdop_types::BBox;
use fdop_types::store::{FlightLeg, TrackPoint};
use fdop_types::time::{iso_utc, parse_iso_utc};

struct Ref {
    icao: &'static str,
    t: f64,
    lat: f64,
    lon: f64,
    alt_m: f64,
    gs: f64,
    track: f64,
}

fn interp(l: &FlightLeg, t: f64) -> Option<(TrackPoint, TrackPoint, f64)> {
    let i = l.points.partition_point(|p| p.t <= t);
    if i == 0 || i >= l.points.len() {
        return None;
    }
    let (a, b) = (l.points[i - 1], l.points[i]);
    let w = if b.t > a.t {
        (t - a.t) / (b.t - a.t)
    } else {
        0.0
    };
    Some((a, b, w))
}

fn main() {
    let cache = fdop_types::default_cache_dir();
    let bbox = BBox::new(40.5024, -79.7621, 45.0147, -71.8562).padded_km(30.0);
    let (lat_c, lon_c) = bbox.center();
    // the oracle used N at the recording site
    let (n, src) = geoid_height_or_default(&cache, 40.876466, -73.524951);
    println!("geoid N (site) = {n:.3} m ({src}); bbox centre {lat_c:.3},{lon_c:.3}");
    let spec = IngestSpec {
        bbox,
        t_start: parse_iso_utc("2026-09-08T04:00:00Z").unwrap(),
        t_end: parse_iso_utc("2026-09-10T11:00:00Z").unwrap(),
        geoid_n_m: n,
        keep_unknown: true,
        include_jets: false,
    };
    let opts = FetchOptions {
        offline: true,
        ..Default::default()
    };
    let store = fetch_window(&spec, &cache, &opts).expect("cache-only fetch");
    println!(
        "store: {} legs, {} points, t {} .. {}",
        store.legs.len(),
        store.n_points(),
        iso_utc(store.meta.t_start),
        iso_utc(store.meta.t_end)
    );
    let refs = [
        Ref {
            icao: "a1dfeb",
            t: 1_788_900_800.1,
            lat: 40.864563,
            lon: -73.492126,
            alt_m: 5556.5,
            gs: 136.84,
            track: 59.0,
        },
        Ref {
            icao: "ace391",
            t: 1_788_901_732.06,
            lat: 40.882919,
            lon: -73.568542,
            alt_m: 1083.6,
            gs: 100.47,
            track: 212.9,
        },
    ];
    for r in &refs {
        let leg = store
            .legs
            .iter()
            .find(|l| l.icao == r.icao && l.t0() <= r.t && r.t <= l.t1());
        let Some(l) = leg else {
            println!("{}: NO leg covers {}", r.icao, iso_utc(r.t));
            continue;
        };
        let Some((a, b, w)) = interp(l, r.t) else {
            println!("{}: leg {} has no bracket", r.icao, l.key);
            continue;
        };
        let lerp = |x: f64, y: f64| x + (y - x) * w;
        let lat = lerp(a.lat, b.lat);
        let lon = lerp(a.lon, b.lon);
        let alt = lerp(f64::from(a.alt_m), f64::from(b.alt_m));
        let dn = (lat - r.lat) * 111_195.0;
        let de = (lon - r.lon) * 111_195.0 * r.lat.to_radians().cos();
        println!(
            "{} leg {} {} {} n={} src={} | bracket dt {:.2}s | horiz err {:.1} m, alt {:.1} (ref {:.1}, d {:+.1}) alt_src {}/{} | gs {:.2} (ref {:.2}) trk {:.1} (ref {:.1})",
            r.icao,
            l.key,
            l.reg,
            l.typ,
            l.points.len(),
            l.source,
            b.t - a.t,
            dn.hypot(de),
            alt,
            r.alt_m,
            alt - r.alt_m,
            a.alt_src,
            b.alt_src,
            lerp(f64::from(a.gs_mps), f64::from(b.gs_mps)),
            r.gs,
            a.track_deg,
            r.track
        );
    }
    // live terrain at the recording site (z12)
    {
        use fdop_types::Elevation;
        let dem = fdop_data::TerrariumDem::new(&cache, 12).with_fallback(f64::NAN);
        let t = std::time::Instant::now();
        let e = dem.elev_m(40.876466, -73.524951);
        println!(
            "terrain {} at site: {e:.2} m (fetched {}, failures {}, {:.2}s)",
            dem.describe(),
            dem.fetched(),
            dem.failures(),
            t.elapsed().as_secs_f64()
        );
    }
    // per-day coverage of the cached day stores
    for day in ["2026-09-08", "2026-09-09", "2026-09-10"] {
        let dir = cache.join("adsb").join(day);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "fdtrk")
                && let Ok(s) = fdop_types::store::TrackStore::load(&p)
            {
                let t0 = s.legs.iter().map(|l| l.t0()).fold(f64::INFINITY, f64::min);
                let t1 = s
                    .legs
                    .iter()
                    .map(|l| l.t1())
                    .fold(f64::NEG_INFINITY, f64::max);
                println!(
                    "{}: {} legs, {} pts, data {} .. {}, {} bytes",
                    p.file_name().unwrap().to_string_lossy(),
                    s.legs.len(),
                    s.n_points(),
                    iso_utc(t0),
                    iso_utc(t1),
                    e.metadata().map(|m| m.len()).unwrap_or(0)
                );
            }
        }
    }
}
