//! Command line: `fdop locate | detect | fetch | runs | doctor | selftest`.

use crate::pipeline::{self, LocateArgs, Log};
use clap::{Args, Parser, Subcommand};
use fdop_core::audio::{self, AudioSelection};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fdop",
    version,
    about = "Locate an audio/video recording from the Doppler curves of propeller aircraft flying past, matched against archived ADS-B tracks",
    long_about = "Give it a recording, the date(s) it was made and a coarse US region; it finds the flyovers, fits their Doppler curves, downloads the ADS-B archive for those days, finds the flights and the single place and clock that explain the curves, and writes a report with a map, a confidence radius and the nearest town and ZIP code.\n\nOnly use it on recordings you made or have permission to analyse."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Full pipeline: media + dates + region -> lat/lon, radius, map, town, ZIP.
    Locate(LocateCli),
    /// Detect and measure flyovers only (no ADS-B); writes events.json, traces and figures.
    Detect(DetectCli),
    /// Pre-download and cache ADS-B tracks for dates + region.
    Fetch(FetchCli),
    /// List past `locate` runs from the run ledger.
    Runs {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// Cache root holding runs.sqlite (default: $FDOP_CACHE, $FDOP_CACHE_DIR, $XDG_CACHE_HOME/fdop or ~/.cache/fdop).
        #[arg(long)]
        cache: Option<PathBuf>,
    },
    /// Check tools, cache and (optionally) every network service the pipeline uses.
    Doctor {
        /// Also probe the network services (adsb.lol releases, NGS geoid, Census geocoder, tiles).
        #[arg(long)]
        network: bool,
        /// Cache root to check (default: $FDOP_CACHE, $FDOP_CACHE_DIR, $XDG_CACHE_HOME/fdop or ~/.cache/fdop).
        #[arg(long)]
        cache: Option<PathBuf>,
    },
    /// Offline end-to-end test on a synthesised recording with known answer.
    Selftest {
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

#[derive(Args, Debug, Clone)]
pub struct Common {
    /// Cache root (ADS-B day stores, terrain, gazetteer, map tiles). Default: $FDOP_CACHE, $FDOP_CACHE_DIR, $XDG_CACHE_HOME/fdop or ~/.cache/fdop.
    #[arg(long, global = false)]
    pub cache: Option<PathBuf>,
    /// Never use the network (cached data only).
    #[arg(long)]
    pub offline: bool,
    /// Less progress output on stderr.
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Args, Debug, Clone, Copy)]
pub struct AudioCli {
    /// Zero-based audio-stream ordinal (0 is the first audio track).
    #[arg(long, default_value_t = 0)]
    pub audio_stream: usize,
    /// Zero-based channel within the selected audio stream; otherwise downmix all its channels.
    #[arg(long)]
    pub audio_channel: Option<usize>,
}

impl AudioCli {
    fn selection(self) -> AudioSelection {
        AudioSelection {
            stream: self.audio_stream,
            channel: self.audio_channel,
        }
    }
}

#[derive(Args, Debug)]
pub struct LocateCli {
    /// Audio or video file (anything ffmpeg reads).
    pub media: PathBuf,
    /// Local date(s) of the recording: YYYY-MM-DD, or a range YYYY-MM-DD..YYYY-MM-DD (also "D0 to D1" or "D0,D1"; YYYY/MM/DD and YYYYMMDD work too). At most 15 days.
    #[arg(long)]
    pub dates: String,
    /// Coarse US region: "NY", "New York", a place "Oyster Bay, NY", a county "Nassau County, NY", a ZIP, "lat,lon[,km]", "S,W,N,E", or "CONUS".
    #[arg(long)]
    pub near: String,
    /// Output directory (default fdop_runs/<name>_<time>).
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Exact recording start if known (ISO 8601, e.g. 2026-09-08T19:17:30-04:00).
    #[arg(long)]
    pub start: Option<String>,
    /// Finite, nonnegative clock tolerance around --start or the file's creation_time (s).
    #[arg(long, default_value_t = 180.0)]
    pub clock_tol: f64,
    /// Ignore the container's creation_time metadata.
    #[arg(long)]
    pub ignore_metadata_time: bool,
    /// Maximum number of events that drive the pair join (others can still confirm).
    #[arg(long, default_value_t = 12)]
    pub max_events: usize,
    /// Skip the curve match, so the answer comes from the trace search alone.
    #[arg(long)]
    pub no_curve_match: bool,
    /// Print the result JSON on stdout.
    #[arg(long)]
    pub json: bool,
    #[command(flatten)]
    pub audio: AudioCli,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct DetectCli {
    /// Audio or video file (anything ffmpeg reads).
    pub media: PathBuf,
    /// Output directory (default fdop_runs/detect).
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Less progress output on stderr.
    #[arg(short, long)]
    pub quiet: bool,
    #[command(flatten)]
    pub audio: AudioCli,
}

#[derive(Args, Debug)]
pub struct FetchCli {
    /// Local date(s) of the recording: YYYY-MM-DD, or a range YYYY-MM-DD..YYYY-MM-DD (also "D0 to D1" or "D0,D1"; YYYY/MM/DD and YYYYMMDD work too). At most 15 days.
    #[arg(long)]
    pub dates: String,
    /// Coarse US region: "NY", "New York", a place "Oyster Bay, NY", a county "Nassau County, NY", a ZIP, "lat,lon[,km]", "S,W,N,E", or "CONUS".
    #[arg(long)]
    pub near: String,
    /// Extra padding around the region (km).
    #[arg(long, default_value_t = 30.0)]
    pub pad_km: f64,
    #[command(flatten)]
    pub common: Common,
}

/// Add a row to the run ledger; a failure to write it is a warning, not an error.
fn record_run(cache: &std::path::Path, row: &crate::ledger::RunRow) {
    let db = cache.join("runs.sqlite");
    if let Err(e) = crate::ledger::record(&db, row) {
        eprintln!(
            "fdop: warning: could not record the run in {}: {e}",
            db.display()
        );
    }
}

fn cache_of(c: &Common) -> PathBuf {
    c.cache
        .clone()
        .unwrap_or_else(fdop_types::default_cache_dir)
}

pub fn main() -> i32 {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fdop: error: {e}");
            2
        }
    }
}

fn run(cli: Cli) -> Result<i32, String> {
    match cli.cmd {
        Cmd::Locate(l) => {
            let log = Log::new(l.common.quiet);
            let cache = cache_of(&l.common);
            let run_dir = l
                .out
                .clone()
                .unwrap_or_else(|| pipeline::default_run_dir(&l.media));
            let args = LocateArgs {
                media: l.media.clone(),
                audio_selection: l.audio.selection(),
                dates: l.dates.clone(),
                near: l.near.clone(),
                // fixed here, so a run that fails part-way can still name its partial outputs
                out: Some(run_dir.clone()),
                cache: cache.clone(),
                offline: l.common.offline,
                start: l.start.clone(),
                clock_tol_s: l.clock_tol,
                ignore_metadata_time: l.ignore_metadata_time,
                max_events: l.max_events,
                curve_match: !l.no_curve_match,
                sr: 4000,
                quiet: l.common.quiet,
            };
            let (started, t0) = (pipeline::unix_now(), std::time::Instant::now());
            let out = match pipeline::locate(&args, &log) {
                Ok(o) => o,
                Err(e) => {
                    // the directory counts only if the run got far enough to write into it
                    let wrote = std::fs::read_dir(&run_dir).is_ok_and(|mut d| d.next().is_some());
                    // runs that stop with an error are recorded too, so `fdop runs` lists every attempt
                    record_run(
                        &cache,
                        &crate::ledger::RunRow {
                            created_utc: fdop_types::time::iso_utc(started),
                            input_file: l.media.display().to_string(),
                            dates: l.dates.clone(),
                            region: l.near.clone(),
                            confidence: "error".into(),
                            headline: e.clone(),
                            run_dir: if wrote {
                                run_dir.display().to_string()
                            } else {
                                String::new()
                            },
                            total_s: t0.elapsed().as_secs_f64(),
                            ..Default::default()
                        },
                    );
                    // without -o the directory name has a timestamp the user never saw
                    return Err(if l.out.is_none() && wrote {
                        format!("{e} (partial outputs in {})", run_dir.display())
                    } else {
                        e
                    });
                }
            };
            let a = &out.report.answer;
            let headline = out
                .report
                .place
                .as_ref()
                .map(|p| p.headline.clone())
                .unwrap_or_default();
            record_run(
                &cache,
                &crate::ledger::RunRow {
                    created_utc: out.report.created_utc.clone(),
                    input_file: out.report.input_file.clone(),
                    dates: out.report.dates.clone(),
                    region: out.report.region.clone(),
                    lat: a.lat.is_finite().then_some(a.lat),
                    lon: a.lon.is_finite().then_some(a.lon),
                    radius_m: a.radius_m.is_finite().then_some(a.radius_m),
                    confidence: a.confidence.clone(),
                    headline: headline.clone(),
                    run_dir: out.run_dir.display().to_string(),
                    total_s: out
                        .report
                        .timings
                        .get("total_s")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                },
            );
            if l.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out.report).unwrap_or_default()
                );
            } else if out.found {
                let zip = out
                    .report
                    .place
                    .as_ref()
                    .and_then(|p| p.zip.clone())
                    .unwrap_or_else(|| "?".into());
                println!("location   {:.6}, {:.6}", a.lat, a.lon);
                println!("radius     {:.0} m  ({})", a.radius_m, a.confidence);
                println!("place      {headline}");
                println!("zip        {zip}");
                println!("verdict    {}", a.verdict);
                println!("report     {}", out.run_dir.join("report.html").display());
                println!("map        {}", out.run_dir.join("map.png").display());
            } else {
                println!("no location: {}", a.verdict);
                println!("report     {}", out.run_dir.join("report.html").display());
            }
            Ok(if out.found { 0 } else { 1 })
        }
        Cmd::Detect(d) => {
            let log = Log::new(d.quiet);
            let selection = d.audio.selection();
            let info = audio::probe_with_selection(&d.media, selection)?;
            log.say(&format!(
                "audio: stream {}, {}, {} {} Hz x{}",
                selection.stream,
                selection
                    .channel
                    .map_or_else(|| "mono downmix".to_string(), |n| format!("channel {n}")),
                info.audio_codec,
                info.sample_rate,
                info.channels
            ));
            let out = d
                .out
                .unwrap_or_else(|| PathBuf::from("fdop_runs").join("detect"));
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            let x = audio::decode_with_selection(&d.media, 0.0, None, 4000, selection)?;
            if let Some(w) = pipeline::low_band_warning(&x, 4000) {
                eprintln!("fdop: warning: {w}");
            }
            let dp = fdop_core::detect::DetectParams::default();
            let (ls, lin) = fdop_core::detect::survey(&x, 4000, &dp)?;
            let (dets, det_cut) = fdop_core::detect::detect_counted(&ls, &dp);
            if ls.gain > 1.0 {
                eprintln!(
                    "fdop: warning: survey image spread {:.2} is under {:.2}: raised x{:.2} before detection",
                    ls.scale, dp.min_image_scale, ls.gain
                );
            }
            if det_cut > 0 {
                eprintln!(
                    "fdop: warning: kept the {} strongest of {}{} candidate curves",
                    dp.max_detections,
                    dp.max_detections + det_cut,
                    if det_cut >= 2 * dp.max_detections {
                        "+"
                    } else {
                        ""
                    },
                );
            }
            let mp = fdop_core::measure::MeasureParams::default();
            // As in `locate`: every primary from the silence line up, strongest first, until
            // MAX_MEASURED independent aircraft events survive the search's existing checks.
            let max_measured = fdop_core::measure::MAX_MEASURED;
            let prim: Vec<_> = dets
                .iter()
                .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
                .collect();
            use rayon::prelude::*;
            let evs = fdop_core::measure::measure_ranked_counted(
                prim.len(),
                max_measured,
                |ranks| {
                    prim[ranks.clone()]
                        .par_iter()
                        .enumerate()
                        .map(|(i, det)| {
                            fdop_core::measure::measure(
                                &x,
                                4000,
                                &fdop_core::detect::refine(&lin, det, dp.c),
                                &format!("e{}", ranks.start + i + 1),
                                &mp,
                            )
                        })
                        .collect()
                },
                |events| fdop_core::search::independent_measured_events(events, 0.0).len(),
            );
            if prim.len() > evs.len() {
                eprintln!(
                    "fdop: warning: measured the {} strongest of {} flyover detections, reaching the limit of {max_measured} independent aircraft events",
                    evs.len(),
                    prim.len(),
                );
            }
            std::fs::write(
                out.join("events.json"),
                serde_json::to_string_pretty(&evs).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if let Err(e) = pipeline::survey_png(&ls, &lin, &dets, &evs, &out.join("survey.png")) {
                log.say(&format!("survey.png: {e}"));
            }
            for e in evs.iter().filter(|e| e.fit.is_some()) {
                let _ = pipeline::event_png(e, mp.c, &out.join(format!("{}.png", e.id)), None, "");
                let _ = e.trace.write_csv(&out.join(format!("{}_trace.csv", e.id)));
            }
            for e in &evs {
                match &e.fit {
                    Some(f) => println!(
                        "{:<4} t_rec {:9.2} s  f0 {:7.2} Hz  v {:6.1} m/s  d {:6.0} m  rms {:.2}  {}",
                        e.id,
                        f.best.t0r,
                        f.best.f0,
                        f.best.v,
                        f.best.d,
                        f.best.rmse,
                        if e.quality.ok {
                            "ok".to_string()
                        } else {
                            e.quality.reasons.join("; ")
                        }
                    ),
                    None => println!("{:<4} no fit: {}", e.id, e.quality.reasons.join("; ")),
                }
            }
            log.say(&format!("wrote {}", out.join("events.json").display()));
            Ok(0)
        }
        Cmd::Fetch(f) => {
            let log = Log::new(f.common.quiet);
            let cache = cache_of(&f.common);
            let ctx = fdop_geo::region::RegionContext::new(cache.clone(), !f.common.offline);
            let region =
                fdop_geo::region::parse_region(&f.near, &ctx).map_err(|e| e.to_string())?;
            let (d0, d1) = pipeline::parse_dates(&f.dates)?;
            let (lo, hi) = pipeline::start_window(d0, d1, region.utc_offset_h);
            let (n, geoid_src) = fdop_data::geoid_height_with_options(
                &cache,
                region.center.0,
                region.center.1,
                f.common.offline,
            );
            if geoid_src != "ngs" {
                eprintln!(
                    "fdop: warning: using default geoid height N = {n:.0} m ({})",
                    if f.common.offline {
                        "no usable cached height in --offline mode"
                    } else {
                        "the NGS GEOID lookup failed"
                    }
                );
            }
            let mut spec = fdop_data::IngestSpec {
                bbox: region.bbox.padded_km(f.pad_km),
                t_start: lo - 1200.0,
                t_end: hi + 4.0 * 3600.0,
                geoid_n_m: n,
                keep_unknown: true,
            };
            // as in `locate`: fetch the published part, name the UTC days still pending
            let (until, pending) = fdop_data::published_until(
                spec.t_start,
                spec.t_end,
                &cache,
                f.common.offline,
                pipeline::unix_now(),
            )
            .map_err(|e| format!("ADS-B availability: {e}"))?;
            if !pending.is_empty() {
                let why = format!(
                    "the ADS-B archive for UTC day(s) {} is not published yet{} (adsb.lol releases each UTC day a few hours after it ends)",
                    pending.join(", "),
                    if f.common.offline {
                        " or not cached"
                    } else {
                        ""
                    }
                );
                if until <= spec.t_start {
                    return Err(format!(
                        "{why}; nothing to fetch for {}: rerun later",
                        f.dates
                    ));
                }
                log.say(&format!(
                    "warning: {why}; fetching up to {} only, rerun after the release for the rest",
                    fdop_types::time::iso_utc(until)
                ));
                spec.t_end = until - 1.0;
            }
            let lg = log.clone();
            let opts = fdop_data::FetchOptions {
                offline: f.common.offline,
                progress: Some(std::sync::Arc::new(move |s: &str| lg.say(s))),
                ..Default::default()
            };
            let st = fdop_data::fetch_window(&spec, &cache, &opts).map_err(|e| e.to_string())?;
            println!(
                "{} legs, {} points cached under {}",
                st.legs.len(),
                st.n_points(),
                cache.display()
            );
            Ok(0)
        }
        Cmd::Runs { limit, json, cache } => {
            let rows = crate::ledger::list(
                &cache
                    .unwrap_or_else(fdop_types::default_cache_dir)
                    .join("runs.sqlite"),
                limit,
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).unwrap_or_default()
                );
            } else {
                for r in rows {
                    println!(
                        "{}  {:>10.6} {:>11.6}  r={:>5.0} m  {:<6}  {}  [{} | {} | {}]",
                        r.created_utc,
                        r.lat.unwrap_or(f64::NAN),
                        r.lon.unwrap_or(f64::NAN),
                        r.radius_m.unwrap_or(f64::NAN),
                        r.confidence,
                        r.headline,
                        r.input_file,
                        r.dates,
                        r.region
                    );
                }
            }
            Ok(0)
        }
        Cmd::Doctor { network, cache } => Ok(crate::doctor::run(
            network,
            &cache.unwrap_or_else(fdop_types::default_cache_dir),
        )),
        Cmd::Selftest { out } => {
            let dir = out.unwrap_or_else(|| PathBuf::from("fdop_runs").join("selftest"));
            let r = crate::selftest::run(&dir, &|s| eprintln!("{s}"))?;
            if let Some(why) = &r.no_answer {
                println!("selftest: FAIL | {why}, {} events", r.n_events);
                return Ok(1);
            }
            println!(
                "selftest: {} | error {:.1} m, radius {:.0} m, clock error {:+.2} s, {} events, flights {:?}",
                if r.ok { "PASS" } else { "FAIL" },
                r.err_m,
                r.radius_m,
                r.clock_err_s,
                r.n_events,
                r.flights
            );
            Ok(if r.ok { 0 } else { 1 })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_commands_accept_audio_selection_and_keep_defaults() {
        for subcommand in ["locate", "detect"] {
            let mut args = vec!["fdop", subcommand, "clip.mkv"];
            if subcommand == "locate" {
                args.extend(["--dates", "2026-09-08", "--near", "NY"]);
            }
            for expected in [
                AudioSelection::default(),
                AudioSelection {
                    stream: 2,
                    channel: Some(1),
                },
            ] {
                let parsed = match Cli::try_parse_from(&args).unwrap().cmd {
                    Cmd::Locate(l) => l.audio.selection(),
                    Cmd::Detect(d) => d.audio.selection(),
                    _ => panic!("expected media command"),
                };
                assert_eq!(parsed, expected);
                args.extend(["--audio-stream", "2", "--audio-channel", "1"]);
            }
        }
        assert!(
            Cli::try_parse_from(["fdop", "detect", "clip.mkv", "--audio-channel", "-1"]).is_err()
        );
    }
}
