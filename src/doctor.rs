//! `fdop doctor`: verify tools, cache and (optionally) every remote service.

use std::process::Command;

fn tool(name: &str) -> (bool, String) {
    match Command::new(name).arg("-version").output() {
        Ok(o) if o.status.success() => (
            true,
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string(),
        ),
        Ok(o) => (false, format!("exit {}", o.status)),
        Err(e) => (false, e.to_string()),
    }
}

pub fn run(network: bool, cache: &std::path::Path) -> i32 {
    let mut bad = 0;
    let mut line = |ok: bool, what: &str, detail: &str| {
        println!("[{}] {what}: {detail}", if ok { " ok " } else { "FAIL" });
        if !ok {
            bad += 1;
        }
    };
    for t in ["ffmpeg", "ffprobe"] {
        let (ok, d) = tool(t);
        line(ok, t, &d);
    }
    let w = std::fs::create_dir_all(cache).and_then(|_| {
        let p = cache.join(".doctor_probe");
        std::fs::write(&p, b"ok")
    });
    line(
        w.is_ok(),
        "cache",
        &format!(
            "{} {}",
            cache.display(),
            w.err().map(|e| e.to_string()).unwrap_or_default()
        ),
    );
    // dependency smoke (FFT, least squares, k-d tree, frankenpandas, fsqlite)
    let x = vec![1.0, 0.0, -1.0, 0.0];
    let fft = fsci_fft::rfft(&x, &fsci_fft::FftOptions::default()).map(|v| v.len());
    line(fft.is_ok(), "fsci-fft", &format!("{fft:?}"));
    let kd = fsci_spatial::KDTree::new(&[vec![0.0, 0.0], vec![1.0, 1.0]]).is_ok();
    line(kd, "fsci-spatial", "kd-tree");
    // a real write + read round trip through fsqlite in a scratch database under the cache
    let db = cache.join(".doctor_ledger.sqlite");
    let probe = crate::ledger::RunRow {
        created_utc: "doctor".into(),
        input_file: "doctor-probe".into(),
        ..Default::default()
    };
    let lr = crate::ledger::record(&db, &probe).and_then(|_| crate::ledger::list(&db, 1));
    let ok = lr
        .as_ref()
        .is_ok_and(|v| v.iter().any(|r| r.input_file == "doctor-probe"));
    line(
        ok,
        "fsqlite",
        &match &lr {
            Ok(v) => format!("wrote and read back {} row(s) in {}", v.len(), db.display()),
            Err(e) => e.clone(),
        },
    );
    let runs = cache.join("runs.sqlite");
    if runs.exists() {
        let r = crate::ledger::list(&runs, 1_000_000);
        line(
            r.is_ok(),
            "run ledger",
            &match &r {
                Ok(v) => format!("{} run(s) in {}", v.len(), runs.display()),
                Err(e) => e.clone(),
            },
        );
    }
    // Census gazetteer tables: downloaded on first use, so a missing one is information, not a
    // failure; offline runs need them for named regions and the place lookup.
    {
        use fdop_geo::gazetteer::{GazKind, Gazetteer};
        let gaz = Gazetteer::new(cache.to_path_buf(), false);
        let kinds = [
            GazKind::Place,
            GazKind::Zcta,
            GazKind::County,
            GazKind::CountySubdivision,
        ];
        let missing: Vec<&str> = kinds
            .iter()
            .filter(|k| !gaz.available_offline(**k))
            .map(|k| k.file_stem())
            .collect();
        if missing.is_empty() {
            println!("[info] gazetteer: all {} Census tables cached", kinds.len());
        } else {
            println!(
                "[info] gazetteer: not cached yet: {} (downloaded on first use; --offline needs them for place, county and ZIP regions and the place names in the report)",
                missing.join(", ")
            );
        }
    }
    if network {
        match fdop_http::Net::new() {
            Ok(net) => {
                let probes: [(&str, String); 4] = [
                    ("adsb.lol globe_history releases", "https://api.github.com/repos/adsblol/globe_history_2026/releases?per_page=1".into()),
                    ("NGS geoid API", "https://geodesy.noaa.gov/api/geoid/ght?lat=40.88&lon=-73.52".into()),
                    ("Census geocoder", fdop_geo::geocode::reverse_url(40.8765, -73.525)),
                    ("Terrarium tiles", "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/10/301/384.png".into()),
                ];
                for (name, url) in probes {
                    match net.get(&url, &[]) {
                        Ok(r) => line(
                            true,
                            name,
                            &format!("HTTP {} ({} bytes)", r.status, r.body.len()),
                        ),
                        Err(e) => line(false, name, &e.to_string()),
                    }
                }
            }
            Err(e) => line(false, "network client", &e.to_string()),
        }
    }
    println!(
        "{}",
        if bad == 0 {
            "doctor: all checks passed"
        } else {
            "doctor: some checks failed"
        }
    );
    i32::from(bad > 0)
}
