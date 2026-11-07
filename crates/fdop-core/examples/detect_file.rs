//! Survey + template-bank detection on a media file: prints detections strongest first.
//! usage: detect_file <media> [sr=4000]
use fdop_core::{audio, detect};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&a[1]);
    let sr: u32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(4000);
    let t = Instant::now();
    let info = audio::probe(&path).expect("probe");
    println!(
        "duration {:.1}s codec {} sr {} creation {:?}",
        info.duration_s, info.audio_codec, info.sample_rate, info.creation_time
    );
    let x = audio::decode(&path, 0.0, None, sr).expect("decode");
    println!(
        "decoded {} samples in {:.1}s",
        x.len(),
        t.elapsed().as_secs_f64()
    );
    let p = detect::DetectParams::default();
    let t = Instant::now();
    let (ls, lin) = detect::survey(&x, sr, &p).expect("survey");
    println!(
        "survey {}x{} in {:.2}s",
        ls.nr(),
        ls.nt(),
        t.elapsed().as_secs_f64()
    );
    if let Ok(out) = std::env::var("FDOP_SURVEY_PNG") {
        let spec = fdop_core::spectro::Spec {
            t: ls.t.clone(),
            f: ls.freqs.clone(),
            db: ls.data.clone(),
            meta: lin.meta.clone(),
        };
        fdop_core::spectro::render_png(
            &spec,
            std::path::Path::new(&out),
            &[],
            -1.0,
            6.0,
            3600,
            480,
        )
        .expect("png");
        println!("wrote {out}");
    }
    let t = Instant::now();
    let dets = detect::detect(&ls, &p);
    println!("detect {} in {:.2}s", dets.len(), t.elapsed().as_secs_f64());
    for d in &dets {
        let r = detect::refine(&lin, d, p.c);
        println!(
            "#{:<3} t0r={:8.1} f0={:7.2} v={:4.0} d={:6.0} score={:5.2} (a {:5.2} r {:5.2}) harm_of={:?} harms={:?} | refined t0r={:8.2} f0={:7.2} v={:5.1} d={:6.0} lin={:.2}",
            d.id,
            d.t0r,
            d.f0,
            d.v,
            d.d,
            d.score,
            d.score_approach,
            d.score_recede,
            d.harmonic_of,
            d.harmonics
                .iter()
                .map(|x| (x * 100.0).round() / 100.0)
                .collect::<Vec<_>>(),
            r.t0r,
            r.f0,
            r.v,
            r.d,
            r.score
        );
    }
    let mp = fdop_core::measure::MeasureParams::default();
    let t = Instant::now();
    let prim: Vec<_> = dets
        .iter()
        .filter(|d| d.harmonic_of.is_none() && d.score >= p.thr_db)
        .take(16)
        .collect();
    use rayon::prelude::*;
    let evs: Vec<_> = prim
        .par_iter()
        .map(|d| {
            fdop_core::measure::measure_family(&x, sr, &lin, d, &dets, &format!("d{}", d.id), &mp)
        })
        .collect();
    println!(
        "measured {} in {:.2}s",
        evs.len(),
        t.elapsed().as_secs_f64()
    );
    if let Ok(dir) = std::env::var("FDOP_EV_PNG_DIR") {
        for e in &evs {
            if let (Some(sp), Some(f)) = (&e.spec, &e.fit) {
                let b = f.best;
                let t0e = b.t0r - b.d / p.c;
                let curve: Vec<(f64, f64)> =
                    sp.t.iter()
                        .step_by(5)
                        .map(|&t| (t, fdop_core::physics::cv_curve(t, b.f0, b.v, b.d, t0e, p.c)))
                        .collect();
                let tr: Vec<(f64, f64)> = e
                    .trace
                    .t
                    .iter()
                    .zip(&e.trace.f)
                    .map(|(a, b)| (*a, *b))
                    .collect();
                let ov = [
                    fdop_core::spectro::Overlay {
                        points: curve,
                        color: [0, 255, 255],
                        dots: false,
                    },
                    fdop_core::spectro::Overlay {
                        points: tr,
                        color: [255, 255, 255],
                        dots: true,
                    },
                ];
                let path = std::path::Path::new(&dir).join(format!("{}.png", e.id));
                fdop_core::spectro::render_png(sp, &path, &ov, -3.0, 25.0, 1800, 700).expect("png");
            }
        }
    }
    for e in &evs {
        let q = &e.quality;
        match &e.fit {
            Some(f) => println!(
                "{:<4} ok={} t0r={:8.2} f0={:7.2} v={:6.1} d={:6.0} rmse={:.2} n={} sides={:.0}/{:.0} snr={:.1} q={:.1} gen_v={:.0}-{:.0} gen_d={:.0}-{:.0} gen_t={:.1}-{:.1} win={:.2} sig={:.2} mis={:.1} {:?} warn={:?}",
                e.id,
                q.ok,
                f.best.t0r,
                f.best.f0,
                f.best.v,
                f.best.d,
                f.best.rmse,
                q.n_points,
                q.span_approach_s,
                q.span_recede_s,
                q.median_snr_db,
                q.score,
                f.ranges_generous.v[0],
                f.ranges_generous.v[1],
                f.ranges_generous.d[0],
                f.ranges_generous.d[1],
                f.ranges_generous.t0r[0],
                f.ranges_generous.t0r[1],
                e.win_s,
                f.diagnostics.local_sigma,
                f.diagnostics.misfit,
                q.reasons,
                f.warnings
                    .iter()
                    .map(|w| &w[..w.len().min(60)])
                    .collect::<Vec<_>>()
            ),
            None => println!("{:<4} ok=false no fit {:?}", e.id, q.reasons),
        }
    }
}
