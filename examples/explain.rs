//! Diagnose how well named flights explain measured events at a given observer and clock.
//!
//! usage: explain <run_dir> <store.fdtrk>[,<store2>...] <lat> <lon> <t_start_utc> <event:icao>...
//!
//! For each pair it prints the residual (trace - f0 * ratio) statistics in time bins for a few
//! speeds of sound, and writes `<run_dir>/explain_<event>.csv`.
use fdop_core::track::{self, TrackModel};
use fdop_types::geo::{dot, geodetic_to_ecef, norm, sub};
use fdop_types::store::TrackStore;

fn read_trace(p: &std::path::Path) -> (Vec<f64>, Vec<f64>) {
    let s = std::fs::read_to_string(p).expect("trace csv");
    let mut t = Vec::new();
    let mut f = Vec::new();
    for l in s.lines().skip(1) {
        let v: Vec<f64> = l.split(',').filter_map(|x| x.parse().ok()).collect();
        if v.len() >= 2 {
            t.push(v[0]);
            f.push(v[1]);
        }
    }
    (t, f)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let run = std::path::PathBuf::from(&a[1]);
    let stores: Vec<TrackStore> = a[2]
        .split(',')
        .map(|p| TrackStore::load(std::path::Path::new(p)).expect("store"))
        .collect();
    let (lat, lon, ts): (f64, f64, f64) = (
        a[3].parse().unwrap(),
        a[4].parse().unwrap(),
        a[5].parse().unwrap(),
    );
    let elev = 30.0;
    let x = geodetic_to_ecef(lat, lon, elev + fdop_types::MIC_HEIGHT_M);
    let bp = track::BuildParams::default();
    for pair in &a[6..] {
        let (ev, icao) = pair.split_once(':').expect("event:icao");
        let (tt, ff) = read_trace(&run.join(format!("{ev}_trace.csv")));
        if icao == "*" {
            scan(&run, ev, &tt, &ff, &stores, x, ts, &bp);
            continue;
        }
        let tracks: Vec<TrackModel> = stores
            .iter()
            .flat_map(|s| s.legs.iter())
            .filter(|l| l.icao.eq_ignore_ascii_case(icao))
            .flat_map(|l| track::build(l, &bp))
            .collect();
        let tmid = ts + tt[tt.len() / 2];
        let Some(tr) = tracks
            .iter()
            .find(|t| t.t0() < tmid - 60.0 && t.t1() > tmid + 60.0)
        else {
            println!("{ev}:{icao}: no track covers t={tmid:.0}");
            continue;
        };
        let cpa = tr.cpa_near(x, tmid - 20.0, 300.0).expect("cpa");
        if std::env::var("EXPLAIN_DUMP").is_ok() {
            for l in stores
                .iter()
                .flat_map(|s| s.legs.iter())
                .filter(|l| l.icao.eq_ignore_ascii_case(icao))
            {
                for q in l.points.iter().filter(|q| (q.t - cpa.t_e).abs() < 200.0) {
                    let xq = geodetic_to_ecef(q.lat, q.lon, q.alt_m as f64);
                    println!(
                        "   pt t={:+7.1} lat {:.5} lon {:.5} alt {:6.0} gs {:5.1} trk {:5.1} vr {:+5.1} src {} flags {} dist {:6.0}",
                        q.t - cpa.t_e,
                        q.lat,
                        q.lon,
                        q.alt_m,
                        q.gs_mps,
                        q.track_deg,
                        q.vrate_mps,
                        q.alt_src,
                        q.flags,
                        norm(sub(xq, x))
                    );
                }
            }
        }
        let ((_, _, alt), _) = tr.geo_at(cpa.t_e);
        println!(
            "{ev}:{icao} {} {} d={:.0} speed={:.1} alt={:.0} heard={:.2} (t_rec {:.2}) mlat {:.2}",
            tr.key,
            tr.typ,
            cpa.d,
            cpa.speed,
            alt,
            cpa.t_r(343.0),
            cpa.t_r(343.0) - ts,
            tr.mlat_frac
        );
        for c in [343.0, 337.0, 333.0, 329.0, 325.0] {
            // best clock shift and f0 for this c
            let mut best = (f64::INFINITY, 0.0, 0.0);
            let mut dt = -12.0;
            while dt <= 12.0 {
                let mut rr = Vec::with_capacity(tt.len());
                let mut warm = None;
                for &t in &tt {
                    let r = tr
                        .solve_emission(ts + dt + t, x, c, warm)
                        .map(|(te, _, vr)| {
                            warm = Some(te);
                            c / (c + vr)
                        });
                    rr.push(r.unwrap_or(f64::NAN));
                }
                let (mut a1, mut b1) = (0.0, 0.0);
                for k in 0..tt.len() {
                    if rr[k].is_finite() {
                        a1 += ff[k] * rr[k];
                        b1 += rr[k] * rr[k];
                    }
                }
                let f0 = a1 / b1;
                let mut s = 0.0;
                let mut n = 0;
                for k in 0..tt.len() {
                    if rr[k].is_finite() {
                        s += (ff[k] - f0 * rr[k]).powi(2);
                        n += 1;
                    }
                }
                let rms = (s / n.max(1) as f64).sqrt();
                if rms < best.0 {
                    best = (rms, dt, f0);
                }
                dt += 0.1;
            }
            println!(
                "  c={c:.0}: rms {:.3} Hz at clock shift {:+.1} s, f0 {:.2}",
                best.0, best.1, best.2
            );
            if c == 343.0 || c == 329.0 {
                let mut out = String::from("t_rec,f,model,resid,vr\n");
                let mut bins: Vec<(f64, f64, usize)> = vec![(0.0, 0.0, 0); 12];
                let (t0, t1) = (tt[0], tt[tt.len() - 1]);
                for k in 0..tt.len() {
                    if let Some((te, _, vr)) = tr.solve_emission(ts + best.1 + tt[k], x, c, None) {
                        let m = best.2 * c / (c + vr);
                        let (p, _) = tr.pv(te);
                        let _ = dot(sub(p, x), sub(p, x));
                        let _ = norm(sub(p, x));
                        out.push_str(&format!(
                            "{:.2},{:.3},{:.3},{:.3},{:.2}\n",
                            tt[k],
                            ff[k],
                            m,
                            ff[k] - m,
                            vr
                        ));
                        let b =
                            (((tt[k] - t0) / (t1 - t0 + 1e-9)) * 12.0).floor().min(11.0) as usize;
                        bins[b].0 += ff[k] - m;
                        bins[b].1 += (ff[k] - m).powi(2);
                        bins[b].2 += 1;
                    }
                }
                let line: Vec<String> = bins
                    .iter()
                    .map(|b| {
                        if b.2 > 0 {
                            format!("{:+.2}", b.0 / b.2 as f64)
                        } else {
                            "  .  ".into()
                        }
                    })
                    .collect();
                println!("    mean resid by time bin: {}", line.join(" "));
                std::fs::write(run.join(format!("explain_{ev}_c{c:.0}.csv")), out).unwrap();
            }
        }
    }
}

/// Scan every track for the best explanation of event `ev` at observer x and clock ts.
#[allow(clippy::too_many_arguments)]
fn scan(
    run: &std::path::Path,
    ev: &str,
    tt: &[f64],
    ff: &[f64],
    stores: &[TrackStore],
    x: fdop_types::geo::V3,
    ts: f64,
    bp: &track::BuildParams,
) {
    let evs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run.join("events.json")).unwrap()).unwrap();
    let e = evs
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == ev)
        .unwrap();
    let b = &e["fit"]["best"];
    let (t0r, f0, v, d) = (
        b["t0r"].as_f64().unwrap(),
        b["f0"].as_f64().unwrap(),
        b["v"].as_f64().unwrap(),
        b["d"].as_f64().unwrap(),
    );
    let t_heard = ts + t0r;
    let mut res: Vec<(f64, String)> = Vec::new();
    for l in stores.iter().flat_map(|s| s.legs.iter()) {
        if l.points.first().is_none_or(|p| p.t > t_heard + 60.0)
            || l.points.last().is_none_or(|p| p.t < t_heard - 400.0)
        {
            continue;
        }
        for tr in track::build(l, bp) {
            let c = 337.0;
            let Some(cpa) = tr.cpa_near(x, t_heard - 10.0, 120.0) else {
                continue;
            };
            if cpa.d > 15000.0 || (cpa.t_r(c) - t_heard).abs() > 40.0 || !cpa.interior {
                continue;
            }
            let mut best = (f64::INFINITY, 0.0, 0.0);
            let mut dt = -6.0;
            while dt <= 6.0 {
                let rr: Vec<f64> = tt
                    .iter()
                    .map(|&t| {
                        tr.solve_emission(ts + dt + t, x, c, None)
                            .map_or(f64::NAN, |(_, _, vr)| c / (c + vr))
                    })
                    .collect();
                let (mut a1, mut b1) = (0.0, 0.0);
                for k in 0..tt.len() {
                    if rr[k].is_finite() {
                        a1 += ff[k] * rr[k];
                        b1 += rr[k] * rr[k];
                    }
                }
                let f0b = a1 / b1;
                let (mut s, mut n) = (0.0, 0usize);
                for k in 0..tt.len() {
                    let r2 = if rr[k].is_finite() {
                        (ff[k] - f0b * rr[k]).powi(2).min(25.0)
                    } else {
                        25.0
                    };
                    s += r2;
                    n += 1;
                }
                let rms = (s / n.max(1) as f64).sqrt();
                if rms < best.0 {
                    best = (rms, dt, f0b);
                }
                dt += 0.5;
            }
            res.push((
                best.0,
                format!(
                    "{} {} {} d={:.0} v={:.1} dt={:+.1} f0={:.2} rms(cap5)={:.2}",
                    tr.key,
                    tr.typ,
                    tr.engine.as_str(),
                    cpa.d,
                    cpa.speed,
                    best.1,
                    best.2,
                    best.0
                ),
            ));
        }
    }
    res.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!(
        "{ev}: fit t0r {t0r:.1} f0 {f0:.2} v {v:.1} d {d:.0}; {} candidate passes",
        res.len()
    );
    for r in res.iter().take(4) {
        println!("    {}", r.1);
    }
}
