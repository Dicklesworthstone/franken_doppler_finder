//! Validation aid for a recording whose place and clock are known: list every archived flight
//! that passed near the known observer while it was recording, and pair those passes with the
//! detected events (by heard-CPA time), so a miss can be traced to "nothing flew by", "the
//! detector missed it" or "the search mis-scored it".
//!
//! usage: truth <run_dir> <store.fdtrk>[,<store2>...] <lat> <lon> <t_start_utc> [elev_m] [max_km]
//!
//! `t_start_utc` is ISO 8601 or unix seconds; `elev_m` (default 0) is the ground elevation;
//! `max_km` (default 8) the slant-distance cut. Reads `<run_dir>/events.json`; without one it
//! lists the passes of the 24 h after `t_start` (to pick test windows with close traffic).
use fdop_types::geo::{geodetic_to_ecef, norm, sub};
use fdop_types::store::TrackStore;
use serde_json::Value;

const C: f64 = 343.0;

struct Pass {
    t_heard_rec: f64,
    d_m: f64,
    alt_m: f64,
    gs: f64,
    label: String,
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!(
            "usage: truth <run_dir> <store.fdtrk>[,...] <lat> <lon> <t_start_utc> [elev_m] [max_km]"
        );
        std::process::exit(2);
    }
    let run = std::path::PathBuf::from(&a[1]);
    let (lat, lon): (f64, f64) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let t_start = a[5]
        .parse::<f64>()
        .ok()
        .or_else(|| fdop_types::time::parse_iso_utc(&a[5]).ok())
        .expect("t_start: ISO 8601 or unix seconds");
    let elev: f64 = a.get(6).map_or(0.0, |s| s.parse().unwrap());
    let max_m: f64 = a.get(7).map_or(8.0, |s| s.parse().unwrap()) * 1000.0;
    // without events.json (e.g. scanning a whole day for good test windows) list passes only
    let evs: Value = std::fs::read(run.join("events.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Array(Vec::new()));
    let evs: Vec<(String, f64, f64, bool)> = evs
        .as_array()
        .expect("array")
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap_or("?").to_string(),
                e["detection"]["t0r"].as_f64().unwrap_or(f64::NAN),
                e["detection"]["f0"].as_f64().unwrap_or(f64::NAN),
                e["quality"]["ok"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    let dur = if evs.is_empty() {
        86_400.0
    } else {
        evs.iter().map(|e| e.1).fold(0.0, f64::max) + 120.0
    };
    let o = geodetic_to_ecef(lat, lon, elev + fdop_types::MIC_HEIGHT_M);
    let mut passes = Vec::new();
    for p in a[2].split(',') {
        let s = TrackStore::load(std::path::Path::new(p)).expect("store");
        for l in &s.legs {
            let mut best: Option<(f64, f64, f64, f64)> = None;
            for q in &l.points {
                if q.t < t_start - 120.0 || q.t > t_start + dur + 120.0 || !q.alt_m.is_finite() {
                    continue;
                }
                let d = norm(sub(geodetic_to_ecef(q.lat, q.lon, q.alt_m as f64), o));
                if best.is_none_or(|b| d < b.1) {
                    best = Some((q.t, d, q.alt_m as f64, q.gs_mps as f64));
                }
            }
            if let Some((t, d, alt, gs)) = best
                && d <= max_m
            {
                passes.push(Pass {
                    t_heard_rec: t + d / C - t_start,
                    d_m: d,
                    alt_m: alt,
                    gs,
                    label: format!(
                        "{} {} {} [{}; {}]",
                        l.icao,
                        if l.reg.is_empty() { "-" } else { &l.reg },
                        if l.typ.is_empty() { "?" } else { &l.typ },
                        l.engine.as_str(),
                        l.source
                    ),
                });
            }
        }
    }
    passes.sort_by(|x, y| x.t_heard_rec.total_cmp(&y.t_heard_rec));
    println!(
        "{} archived passes within {:.0} km of {lat:.5},{lon:.5} (rec time = heard CPA - start):",
        passes.len(),
        max_m / 1000.0
    );
    println!("  t_rec(s)  slant(m)  alt(m)  gs(m/s)  flight                         nearest event");
    for p in &passes {
        let near = evs
            .iter()
            .filter(|e| (e.1 - p.t_heard_rec).abs() <= 45.0)
            .min_by(|x, y| {
                (x.1 - p.t_heard_rec)
                    .abs()
                    .total_cmp(&(y.1 - p.t_heard_rec).abs())
            })
            .map_or("-".to_string(), |e| {
                format!(
                    "{} (t {:.0}, {:+.0} s, f0 {:.0} Hz{})",
                    e.0,
                    e.1,
                    e.1 - p.t_heard_rec,
                    e.2,
                    if e.3 { "" } else { ", failed quality" }
                )
            });
        println!(
            "  {:8.0}  {:8.0}  {:6.0}  {:7.1}  {:30} {near}",
            p.t_heard_rec, p.d_m, p.alt_m, p.gs, p.label
        );
    }
    println!("events without an archived pass within 45 s:");
    for e in &evs {
        if !passes.iter().any(|p| (e.1 - p.t_heard_rec).abs() <= 45.0) {
            println!(
                "  {} t {:.0} f0 {:.0} Hz{}",
                e.0,
                e.1,
                e.2,
                if e.3 { "" } else { " (failed quality)" }
            );
        }
    }
}
