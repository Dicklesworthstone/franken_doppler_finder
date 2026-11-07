//! Offline end-to-end self-test: synthesise a recording of two propeller flyovers heard from a
//! known point (plus decoy flights and a stationary engine tone), write it as WAV, then run the
//! real chain (ffmpeg decode -> detect -> measure -> search -> refine) and check the answer.

use fdop_core::search::{SearchEvent, SearchInputs, SearchParams};
use fdop_core::track::{Prop, TrackModel, straight_track};
use fdop_core::{audio, detect, measure};
use fdop_types::atmo::Atmosphere;
use fdop_types::geo::{Enu, haversine_m};
use fdop_types::{BBox, FlatElevation, MIC_HEIGHT_M};
use rayon::prelude::*;
use std::io::Write;
use std::path::{Path, PathBuf};

fn write_wav(path: &Path, x: &[f32], sr: u32) -> std::io::Result<()> {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    let n = x.len() as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + n * 2).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&sr.to_le_bytes())?;
    w.write_all(&(sr * 2).to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&(n * 2).to_le_bytes())?;
    for &v in x {
        w.write_all(&((v.clamp(-1.0, 1.0) * 32000.0) as i16).to_le_bytes())?;
    }
    w.flush()
}

pub struct SelftestResult {
    pub err_m: f64,
    pub radius_m: f64,
    pub clock_err_s: f64,
    pub n_events: usize,
    pub flights: Vec<String>,
    pub ok: bool,
    /// Why there is no answer to grade (the search returned no candidate).
    pub no_answer: Option<String>,
}

pub(crate) struct SyntheticRecording {
    pub wav: PathBuf,
    pub tracks: Vec<TrackModel>,
    pub observer: (f64, f64),
    pub start: f64,
    pub duration: f64,
    pub sample_rate: u32,
}

/// The same known recording for the selftest and offline pipeline regressions. The caller
/// supplies the atmosphere its real search will use, so fixtures never hide a medium mismatch.
pub(crate) fn synthesize(
    dir: &Path,
    atmo: &dyn Atmosphere,
    say: &(dyn Fn(&str) + Sync),
) -> Result<SyntheticRecording, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (lat0, lon0) = (40.8765, -73.5250);
    let enu = Enu::new(lat0, lon0, 0.0);
    let x_obs = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
    let t_s = 1_788_895_000.0;
    let dur = 1300.0;
    let mk = |key: &str, typ: &str, p0: [f64; 3], v: [f64; 3], t0: f64| -> TrackModel {
        let mut t = straight_track(&enu, p0, v, t_s + t0, t_s - 600.0, t_s + dur + 600.0, 2.0);
        t.key = key.into();
        t.icao = key.to_lowercase();
        t.typ = typ.into();
        t
    };
    let tracks = vec![
        mk(
            "A1DFEB",
            "PC12",
            [0.0, 5000.0, 1500.0],
            [137.0, 0.0, 0.0],
            400.0,
        ),
        mk(
            "ACE391",
            "BE20",
            [2500.0, 0.0, 900.0],
            [0.0, 104.0, 0.0],
            900.0,
        ),
        // decoys: similar speeds, different places/times
        mk(
            "DECOY1",
            "C172",
            [-7000.0, -3000.0, 800.0],
            [0.0, 55.0, 0.0],
            700.0,
        ),
        mk(
            "DECOY2",
            "PC12",
            [0.0, -12000.0, 1800.0],
            [-135.0, 0.0, 0.0],
            420.0,
        ),
        mk(
            "DECOY3",
            "BE20",
            [9000.0, 0.0, 1000.0],
            [0.0, -100.0, 0.0],
            880.0,
        ),
    ];
    let sr = 4000u32;
    let n = (dur * sr as f64) as usize;
    // audible sources: A (f0 112.7) and B (f0 129.5); decoys are far and silent
    let srcs = [(0usize, 112.7f64, 0.10f64), (1usize, 129.5, 0.12)];
    say("selftest: synthesising 1300 s of audio (two flyovers, a stepped ground tone, noise)");
    let chunks: Vec<Vec<f32>> = srcs
        .par_iter()
        .map(|&(ti, f0, amp)| {
            let tr = &tracks[ti];
            // medium for the altitude at closest approach (a straight ENU line rises with range)
            let t_cpa = tr
                .cpa_near(x_obs, t_s + 650.0, 700.0)
                .map_or(t_s, |c| c.t_e);
            let (_, _, h) = fdop_types::geo::ecef_to_geodetic(tr.pos_at(t_cpa));
            let prop = Prop::from_medium(&atmo.medium(lat0, lon0, t_s, MIC_HEIGHT_M, h), &enu);
            let mut out = vec![0f32; n];
            let mut phase = 0.0f64;
            let mut warm = None;
            for (i, o) in out.iter_mut().enumerate() {
                let tu = t_s + i as f64 / sr as f64;
                if let Some((te, r, ratio)) = tr.solve_emission_p(tu, x_obs, &prop, warm) {
                    warm = Some(te);
                    let f = f0 * ratio;
                    phase += 2.0 * std::f64::consts::PI * f / sr as f64;
                    let a = amp * (3000.0 / r).min(3.0);
                    *o = (a * (phase.sin() + 0.6 * (2.0 * phase).sin())) as f32;
                }
            }
            out
        })
        .collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut x = vec![0f32; n];
    for (i, v) in x.iter_mut().enumerate() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let t = i as f64 / sr as f64;
        let ground = if (200.0..500.0).contains(&t) {
            0.05 * (2.0 * std::f64::consts::PI * 61.0 * t).sin()
        } else {
            0.0
        };
        *v = chunks[0][i]
            + chunks[1][i]
            + ground as f32
            + 0.5 * ((seed as f64 / u64::MAX as f64) * 2.0 - 1.0) as f32;
    }
    let wav = dir.join("selftest.wav");
    write_wav(&wav, &x, sr).map_err(|e| e.to_string())?;
    say(&format!("selftest: wrote {}", wav.display()));
    Ok(SyntheticRecording {
        wav,
        tracks,
        observer: (lat0, lon0),
        start: t_s,
        duration: dur,
        sample_rate: sr,
    })
}

pub fn run(dir: &Path, say: &(dyn Fn(&str) + Sync)) -> Result<SelftestResult, String> {
    // A warm afternoon with a north-westerly breeze: the search must model both.
    let atmo = fdop_types::atmo::StandardAtmosphere::with_wind(26.0, 0.0, 0.4, 5.0, -4.0);
    let SyntheticRecording {
        wav,
        tracks,
        observer: (lat0, lon0),
        start: t_s,
        duration: dur,
        sample_rate: sr,
    } = synthesize(dir, &atmo, say)?;
    let c = fdop_types::C_SOUND;
    let xd = audio::decode(&wav, 0.0, None, sr)?;
    let dp = detect::DetectParams::default();
    let (ls, lin) = detect::survey(&xd, sr, &dp)?;
    let dets = detect::detect(&ls, &dp);
    let mp = measure::MeasureParams::default();
    let evs: Vec<_> = dets
        .iter()
        .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_db)
        .take(8)
        .enumerate()
        .map(|(k, d)| {
            measure::measure(
                &xd,
                sr,
                &detect::refine(&lin, d, c),
                &format!("e{}", k + 1),
                &mp,
            )
        })
        .collect();
    for e in &evs {
        if let Some(f) = &e.fit {
            say(&format!(
                "selftest: {} t_rec={:.2} f0={:.2} v={:.1} d={:.0} ok={}",
                e.id, f.best.t0r, f.best.f0, f.best.v, f.best.d, e.quality.ok
            ));
        }
    }
    let sevs: Vec<SearchEvent> = evs
        .iter()
        .filter_map(|e| SearchEvent::from_measured(e, 6.0))
        .collect();
    let region = BBox::new(lat0 - 0.3, lon0 - 0.4, lat0 + 0.3, lon0 + 0.4);
    let sp = SearchParams::new(t_s - 6.0 * 3600.0, t_s + 6.0 * 3600.0, dur, region);
    let dem = FlatElevation(0.0);
    let inp = SearchInputs {
        events: sevs.clone(),
        tracks: &tracks,
        heard_times: dets.iter().map(|d| d.t0r).collect(),
        coarse_dem: &dem,
        fine_dem: &dem,
        atmo: &atmo,
    };
    let res = fdop_core::search::run(&inp, &sp, say);
    let Some(top) = res.candidates.first() else {
        // a test that finds nothing has failed (exit 1), like a wrong answer; not a usage error
        return Ok(SelftestResult {
            err_m: f64::NAN,
            radius_m: f64::NAN,
            clock_err_s: f64::NAN,
            n_events: sevs.len(),
            flights: Vec::new(),
            ok: false,
            no_answer: Some(format!("no candidate (funnel {:?})", res.funnel)),
        });
    };
    let (lat, lon, radius) = res
        .refine
        .as_ref()
        .map_or((top.lat, top.lon, f64::NAN), |r| (r.lat, r.lon, r.radius_m));
    let pairs: Vec<(String, String)> = top
        .events
        .iter()
        .map(|m| (m.event.clone(), m.flight.clone()))
        .collect();
    for (la, lo, what) in [(lat0, lon0, "truth"), (lat, lon, "found")] {
        if let Ok(c) = fdop_core::search::explain_at(&inp, &sp, la, lo, t_s, 20.0, &pairs) {
            say(&format!(
                "selftest: at {what}: clock {:+.2} s, {}",
                c.t_start_utc - t_s,
                c.events
                    .iter()
                    .map(|m| format!(
                        "{} chi {:.2} rms {:.3} c {:.1}",
                        m.event, m.chi, m.rms_hz, m.c_path
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    let err = haversine_m(lat, lon, lat0, lon0);
    let flights: Vec<String> = top.events.iter().map(|m| m.flight.clone()).collect();
    let ok = err <= radius.max(100.0)
        && flights.contains(&"A1DFEB".to_string())
        && flights.contains(&"ACE391".to_string())
        && (top.t_start_utc - t_s).abs() < 2.0;
    Ok(SelftestResult {
        err_m: err,
        radius_m: radius,
        clock_err_s: top.t_start_utc - t_s,
        n_events: sevs.len(),
        flights,
        ok,
        no_answer: None,
    })
}
