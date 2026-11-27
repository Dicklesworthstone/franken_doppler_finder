//! Command line: `fdop locate | detect | mute | fetch | runs | doctor | selftest`.

use crate::pipeline::{self, LocateArgs, Log};
use clap::{Args, Parser, Subcommand};
use fdop_core::audio::{self, AudioSelection};
use std::path::PathBuf;

/// Default `--clock-tol` (s). With `--start` the clock window is twice this. The curve match ranks
/// its answer against decoy windows of that width within about +-2 h, and needs
/// `curvematch::MIN_DECOYS` (40) of them to judge, which a window of up to 300 s gets
/// (`curvematch::max_judged_window_s`). The first default, 180 s, gave 34, so a run with a start
/// and no `--clock-tol` could never judge. 120 s is the tolerance the curve match was built and
/// checked on (48 decoys; the benchmark's R2 and R3 inputs), clear of the 150 s edge.
pub const DEFAULT_CLOCK_TOL_S: f64 = 120.0;

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
    /// Export a verified MP4 or mute selected intervals in an audio-only M4A.
    Mute {
        /// Local audio or video recording.
        media: PathBuf,
        /// Use the first audio track of this separate local file as the only soundtrack.
        #[arg(long)]
        soundtrack: Option<PathBuf>,
        /// Mute the selected audio track between START and END seconds on the recording clock (repeatable).
        #[arg(long, value_name = "START..END", conflicts_with = "soundtrack")]
        silence_range: Vec<crate::mute::SilenceRange>,
        /// Original audio track to retain in range mode (zero-based, default 0).
        #[arg(long, value_name = "N", requires = "silence_range")]
        audio_stream: Option<usize>,
        /// New .mp4 file, or an audio-only .m4a file when using --silence-range.
        #[arg(short, long)]
        out: PathBuf,
    },
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
    /// Also match known jets with stable subsonic tones (experimental; uses a separate ADS-B cache).
    #[arg(long)]
    pub include_jets: bool,
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
    /// New or empty output directory (default fdop_runs/<name>_<time>).
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Exact recording start if known (ISO 8601, e.g. 2026-09-08T19:17:30-04:00).
    #[arg(long)]
    pub start: Option<String>,
    /// Finite, nonnegative clock tolerance around --start or the file's creation_time (s). The curve
    /// match can judge a clock window of at most 300 s: with --start, a tolerance of 150 s or less.
    #[arg(long, default_value_t = DEFAULT_CLOCK_TOL_S)]
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
    /// Audit every audio stream, its mono mix and each channel of multichannel audio.
    #[arg(long, conflicts_with_all = ["audio_stream", "audio_channel"])]
    pub all_audio: bool,
    /// Restrict media probing and decoding to local files and embedded data.
    #[arg(long)]
    pub offline: bool,
    /// Print the recording audit as JSON on stdout.
    #[arg(long)]
    pub json: bool,
    /// New or empty output directory (default: a fresh fdop_runs/detect or fdop_runs/detect-all).
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

/// The ledger stores the number from the report without its radius basis.
fn history_location(lat: Option<f64>, lon: Option<f64>, radius: Option<f64>) -> String {
    match (lat, lon, radius) {
        (Some(lat), Some(lon), Some(m)) => {
            format!("{lat:>10.6} {lon:>11.6}  reported radius {m:.0} m (see report for basis)")
        }
        _ => "no location".into(),
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
            let args = LocateArgs {
                media: l.media.clone(),
                audio_selection: l.audio.selection(),
                dates: l.dates.clone(),
                near: l.near.clone(),
                out: l.out.clone(),
                cache: cache.clone(),
                offline: l.common.offline,
                include_jets: l.common.include_jets,
                start: l.start.clone(),
                clock_tol_s: l.clock_tol,
                ignore_metadata_time: l.ignore_metadata_time,
                max_events: l.max_events,
                curve_match: !l.no_curve_match,
                sr: 4000,
                quiet: l.common.quiet,
            };
            let (started, t0) = (pipeline::unix_now(), std::time::Instant::now());
            let mut claimed_dir = None;
            let out = match pipeline::locate_recorded(&args, &log, &mut claimed_dir) {
                Ok(o) => o,
                Err(e) => {
                    // Only this run's own partial artifacts belong in its error ledger row.
                    let claimed_dir = claimed_dir
                        .as_ref()
                        .and_then(|owner| owner.partial_output());
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
                            run_dir: claimed_dir
                                .as_ref()
                                .map(|dir| dir.display().to_string())
                                .unwrap_or_default(),
                            total_s: t0.elapsed().as_secs_f64(),
                            ..Default::default()
                        },
                    );
                    // without -o the directory name has a timestamp the user never saw
                    return Err(
                        if l.out.is_none()
                            && let Some(run_dir) = claimed_dir
                        {
                            format!("{e} (partial outputs in {})", run_dir.display())
                        } else {
                            e
                        },
                    );
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
                println!("{}  ({})", a.radius_status.label(a.radius_m), a.confidence);
                println!("place      {headline}");
                println!("zip        {zip}");
                println!("verdict    {}", a.verdict);
                println!("report     {}", out.run_dir.join("report.html").display());
                if out.report.figures.iter().any(|(_, path)| path == "map.png") {
                    println!("map        {}", out.run_dir.join("map.png").display());
                }
            } else {
                println!("no location: {}", a.verdict);
                println!("report     {}", out.run_dir.join("report.html").display());
            }
            Ok(if out.found { 0 } else { 1 })
        }
        Cmd::Detect(d) => run_detect(d, std::io::stdout().lock()),
        Cmd::Mute {
            media,
            soundtrack,
            silence_range,
            audio_stream,
            out,
        } => {
            crate::mute::export(&media, &out, soundtrack.as_deref(), &silence_range, audio_stream)?;
            if !silence_range.is_empty() {
                println!("Wrote {} with the requested intervals muted on audio stream {}. Review the final recording before sharing.", out.display(), audio_stream.unwrap_or(0));
            } else if soundtrack.is_some() {
                println!("Wrote {} with one replacement soundtrack and no recognized location tags. Review the video and audit the replacement audio before sharing.", out.display());
            } else {
                println!("Wrote {}. Verified zero audio streams and no recognized location tags. Review the video itself before sharing.", out.display());
            }
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
                include_jets: f.common.include_jets,
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
                        "{}  {}  {:<6}  {}  [{} | {} | {}]",
                        r.created_utc,
                        history_location(r.lat, r.lon, r.radius_m),
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

#[derive(serde::Serialize)]
struct MeasuredPassSpan {
    event_id: String,
    first_trace_s: f64,
    last_trace_s: f64,
}

#[derive(serde::Serialize)]
struct AudioAuditSummary {
    audio_stream: usize,
    audio_channel: Option<usize>,
    channels_in_stream: u32,
    decoded_duration_s: f64,
    location_tag_detected: bool,
    independent_pass_count: usize,
    independent_event_ids: Vec<String>,
    measured_pass_spans: Vec<MeasuredPassSpan>,
    warnings: Vec<String>,
}

#[derive(serde::Serialize)]
struct DetectionAudit {
    #[serde(flatten)]
    summary: AudioAuditSummary,
    events: Vec<fdop_core::measure::MeasuredEvent>,
}

fn measure_audio_source(
    probe: &audio::AudioProbe<'_>,
    info: &audio::MediaInfo,
    selection: AudioSelection,
    out: &std::path::Path,
    log: &std::sync::Arc<Log>,
) -> Result<DetectionAudit, String> {
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
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let x = audio::decode_probed(probe, 0.0, None, 4000, selection)?;
    let low_band = pipeline::low_band_warning(&x, 4000);
    if let Some(w) = &low_band {
        eprintln!("fdop: warning: {w}");
    }
    // Use exactly the observations `locate` would search, including its bounded
    // recovery after ordinary measurement finds no aircraft and its final event IDs.
    let measured = pipeline::measure_recording(&x, 4000, log, Some(&out.join("survey.png")))?;
    let decoded_duration_s = x.len() as f64 / 4000.0;
    drop(x);
    for warning in &measured.warnings {
        eprintln!("fdop: warning: {warning}");
    }
    let mut warnings = measured.warnings;
    warnings.extend(low_band);
    let location_tag_detected = info.location_iso6709.is_some();
    if location_tag_detected {
        let note = "the media carries a location tag that could disclose where it was recorded, even after audible flyovers are removed";
        eprintln!("fdop: warning: {note}");
        warnings.push(note.to_string());
    }
    let evs = measured.events;
    std::fs::write(
        out.join("events.json"),
        serde_json::to_string_pretty(&evs).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    for e in evs.iter().filter(|e| e.fit.is_some()) {
        let _ = pipeline::event_png(
            e,
            fdop_types::C_SOUND,
            &out.join(format!("{}.png", e.id)),
            None,
            "",
        );
        let _ = e.trace.write_csv(&out.join(format!("{}_trace.csv", e.id)));
    }
    // A fitted harmonic can pass quality alongside another fit of the same flyover.
    // Use the search's physical eligibility and grouping for the audit count.
    let passes = fdop_core::search::independent_measured_events(&evs, 0.0);
    let ids: Vec<_> = passes.iter().map(|p| p.id.clone()).collect();
    let measured_pass_spans = passes
        .iter()
        .filter_map(|pass| {
            // Search orders the observed points by time. These are the first and
            // last observations, not estimates of when the aircraft became audible.
            let first = *pass.tr_t.first()?;
            let last = *pass.tr_t.last()?;
            (first.is_finite() && last.is_finite()).then(|| MeasuredPassSpan {
                event_id: pass.id.clone(),
                first_trace_s: first,
                last_trace_s: last,
            })
        })
        .collect();
    log.say(&format!(
        "{} independent aircraft passes on the selected audio{}",
        ids.len(),
        if ids.is_empty() {
            String::new()
        } else {
            format!(" ({})", ids.join(", "))
        }
    ));
    Ok(DetectionAudit {
        summary: AudioAuditSummary {
            audio_stream: selection.stream,
            audio_channel: selection.channel,
            channels_in_stream: probe.selected_info(selection)?.channels,
            decoded_duration_s,
            location_tag_detected,
            independent_pass_count: ids.len(),
            independent_event_ids: ids,
            measured_pass_spans,
            warnings,
        },
        events: evs,
    })
}

#[derive(serde::Serialize)]
struct SourceAudit {
    #[serde(flatten)]
    summary: AudioAuditSummary,
    events_file: PathBuf,
}

#[derive(Debug, PartialEq, serde::Serialize)]
struct MeasuredRange {
    start_s: f64,
    end_s: f64,
}

#[derive(serde::Serialize)]
struct TrackRanges {
    audio_stream: usize,
    measured_ranges: Vec<MeasuredRange>,
}

fn measured_ranges<'a>(
    views: impl Iterator<Item = &'a AudioAuditSummary>,
    padding_s: f64,
) -> Result<Vec<MeasuredRange>, String> {
    if !padding_s.is_finite() || padding_s < 0.0 {
        return Err("padding must be a finite, nonnegative number of seconds".into());
    }
    let mut ranges = Vec::new();
    for view in views {
        let duration = view.decoded_duration_s;
        if !duration.is_finite() || duration < 0.0
            || view.measured_pass_spans.len() != view.independent_pass_count
        {
            return Err(format!("incomplete measured timeline for audio stream {}", view.audio_stream));
        }
        for span in &view.measured_pass_spans {
            let first = span.first_trace_s;
            let last = span.last_trace_s;
            if !first.is_finite() || !last.is_finite() || first < 0.0 || last < first
                || first >= duration || last > duration
            {
                return Err(format!("invalid measured span for stream {} event {}", view.audio_stream, span.event_id));
            }
            // Trace timestamps include the last observation. The export uses
            // half-open intervals, so include its 4 kHz sample before padding.
            ranges.push(MeasuredRange {
                start_s: (first - padding_s).max(0.0),
                end_s: (last + 1.0 / 4000.0 + padding_s).min(duration),
            });
        }
    }
    ranges.sort_unstable_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let mut merged = Vec::<MeasuredRange>::new();
    for range in ranges {
        if let Some(prev) = merged.last_mut() && range.start_s <= prev.end_s {
            prev.end_s = prev.end_s.max(range.end_s);
        } else {
            merged.push(range);
        }
    }
    Ok(merged)
}

fn measure_track_views(
    probe: &audio::AudioProbe<'_>,
    info: &audio::MediaInfo,
    out: &std::path::Path,
    log: &std::sync::Arc<Log>,
) -> Result<Vec<(String, AudioAuditSummary)>, String> {
    let mut views = Vec::new();
    let mut visit = |channel: Option<usize>| -> Result<u32, String> {
        let selection = AudioSelection { stream: info.audio_stream, channel };
        let name = match channel {
            Some(n) => format!("stream-{}-channel-{n}", selection.stream),
            None => format!("stream-{}-mix", selection.stream),
        };
        let dir = out.join(&name);
        let audit = measure_audio_source(probe, info, selection, &dir, log)
            .map_err(|e| format!("incomplete audio audit at {name}: {e}"))?;
        let channels = audit.summary.channels_in_stream;
        views.push((name, audit.summary));
        Ok(channels)
    };
    // The mix can cancel opposite-polarity channels. Its full decode also
    // discovers channel layouts that only occur after the stream header.
    let channels = visit(None)?;
    if channels > 1 {
        for channel in 0..channels as usize {
            visit(Some(channel))?;
        }
    }
    Ok(views)
}

fn run_all_audio(d: DetectCli, mut stdout: impl std::io::Write) -> Result<i32, String> {
    let (probe, inventory) = if d.offline {
        audio::AudioProbe::for_offline_audit(&d.media)
    } else {
        audio::AudioProbe::for_audit(&d.media)
    }?;
    let streams = inventory.streams;
    let location_tag_detected = inventory.location_tag_detected;
    let log = Log::new(d.quiet);
    let default = PathBuf::from("fdop_runs").join("detect-all");
    let reservation = pipeline::reserve_output(d.out.as_deref(), &default)?;
    let out = &reservation.path;
    let mut sources = Vec::new();
    let mut tracks = Vec::new();
    for info in &streams {
        let views = measure_track_views(&probe, info, out, &log)
            .map_err(|e| format!("{e} (partial outputs in {})", out.display()))?;
        tracks.push(TrackRanges {
            audio_stream: info.audio_stream,
            measured_ranges: measured_ranges(views.iter().map(|(_, view)| view), 0.0)?,
        });
        sources.extend(views.into_iter().map(|(name, summary)| SourceAudit {
            summary,
            events_file: out.join(name).join("events.json"),
        }));
    }
    if d.json {
        #[derive(serde::Serialize)]
        struct WholeFileAudit {
            audio_stream_count: usize,
            location_tag_detected: bool,
            possible_location_telemetry: Vec<audio::TelemetryTrack>,
            tracks: Vec<TrackRanges>,
            sources: Vec<SourceAudit>,
        }
        let report = WholeFileAudit {
            audio_stream_count: streams.len(),
            location_tag_detected,
            possible_location_telemetry: inventory.possible_location_telemetry,
            tracks,
            sources,
        };
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(|e| e.to_string())?;
        writeln!(stdout).map_err(|e| e.to_string())?;
    } else {
        if streams.is_empty() {
            let metadata = if location_tag_detected {
                "recognized location tag detected"
            } else {
                "no recognized location tag detected"
            };
            writeln!(stdout, "no audio streams; {metadata}").map_err(|e| e.to_string())?;
        }
        for track in inventory.possible_location_telemetry {
            writeln!(
                stdout,
                "warning: data stream {} ({}) may contain location telemetry; GPS samples were not inspected",
                track.stream_index, track.codec_tag
            )
            .map_err(|e| e.to_string())?;
        }
        for track in tracks {
            writeln!(stdout, "stream {}: {} unpadded measured range(s) across all channels (start inclusive, end exclusive)",
                track.audio_stream, track.measured_ranges.len()).map_err(|e| e.to_string())?;
            for range in track.measured_ranges {
                writeln!(stdout, "  {}..{} s", range.start_s, range.end_s).map_err(|e| e.to_string())?;
            }
        }
        for source in sources {
            let s = source.summary;
            let view = s
                .audio_channel
                .map_or_else(|| "mix".to_string(), |n| format!("channel {n}"));
            writeln!(
                stdout,
                "stream {} {view}: {} independent pass(es) [{}]",
                s.audio_stream,
                s.independent_pass_count,
                source.events_file.display()
            )
            .map_err(|e| e.to_string())?;
            for pass in s.measured_pass_spans {
                writeln!(
                    stdout,
                    "  {} measured trace: {:.3} to {:.3} s into recording",
                    pass.event_id, pass.first_trace_s, pass.last_trace_s
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(0)
}

fn run_detect(d: DetectCli, mut stdout: impl std::io::Write) -> Result<i32, String> {
    if d.all_audio {
        return run_all_audio(d, stdout);
    }
    let log = Log::new(d.quiet);
    let selection = d.audio.selection();
    let probe = if d.offline {
        audio::AudioProbe::open_offline(&d.media)
    } else {
        audio::AudioProbe::open(&d.media)
    }?;
    let info = probe.selected_info(selection)?;
    let default = PathBuf::from("fdop_runs").join("detect");
    let reservation = pipeline::reserve_output(d.out.as_deref(), &default)?;
    let out = &reservation.path;
    let audit = measure_audio_source(&probe, &info, selection, out, &log)?;
    log.say(&format!("wrote {}", out.join("events.json").display()));
    if d.json {
        serde_json::to_writer_pretty(&mut stdout, &audit).map_err(|e| e.to_string())?;
        writeln!(stdout).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    for e in &audit.events {
        match &e.fit {
            Some(f) => writeln!(
                stdout,
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
            None => writeln!(
                stdout,
                "{:<4} no fit: {}",
                e.id,
                e.quality.reasons.join("; ")
            ),
        }
        .map_err(|e| e.to_string())?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn history_shows_the_reported_radius_and_omits_absent_coordinates() {
        assert!(
            super::history_location(Some(0.0), Some(0.0), Some(300.0))
                .contains("reported radius 300 m (see report for basis)")
        );
        assert_eq!(super::history_location(None, None, None), "no location");
    }

    use super::*;

    #[test]
    fn jet_catalogue_is_explicit_for_locate_and_fetch() {
        for subcommand in ["locate", "fetch"] {
            let mut args = vec!["fdop", subcommand];
            if subcommand == "locate" {
                args.push("clip.wav");
            }
            args.extend(["--dates", "2026-09-08", "--near", "NY"]);
            for include_jets in [false, true] {
                let actual = match Cli::try_parse_from(&args).unwrap().cmd {
                    Cmd::Locate(l) => l.common.include_jets,
                    Cmd::Fetch(f) => f.common.include_jets,
                    _ => panic!("expected catalogue command"),
                };
                assert_eq!(actual, include_jets);
                args.push("--include-jets");
            }
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn detect_uses_locates_recovered_aircraft_and_event_ids() {
        use fdop_core::{detect, measure, physics, search};
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!(
            "fdop-detect-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The complete 30 s late-pass case from the recording-measurement regression.
        // Float PCM preserves the exact samples across both test containers.
        for (label, amplitude, n_aircraft) in [("fast", 0.32, 1), ("noise", 0.0, 0)] {
            let mut seed = 4242u64;
            let pcm: Vec<f32> = (0..30 * 4000)
                .map(|i| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let t = i as f64 / 4000.0;
                    let tau = physics::retarded_tau(t, 19.37 - 700.0 / 343.0, 245.0, 700.0, 343.0);
                    let range = physics::cv_radial(tau, 245.0, 700.0).0;
                    let tone =
                        amplitude * 700.0 / range * (std::f64::consts::TAU * 83.0 * tau).sin();
                    let noise = 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0);
                    (tone + noise + 0.03 * (std::f64::consts::TAU * 250.0 * t).sin()) as f32
                })
                .collect();
            let raw = dir.join(format!("{label}.f32le"));
            // Matroska carries arbitrary per-stream text tags through FFmpeg.
            let tagged = label == "noise";
            let extension = if tagged { "mka" } else { "wav" };
            let media = dir.join(format!("{label}.{extension}"));
            let out = dir.join(label);
            std::fs::write(
                &raw,
                pcm.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
            )
            .unwrap();
            let mut encoder = Command::new("ffmpeg");
            encoder
                .args([
                    "-nostdin", "-v", "error", "-f", "f32le", "-ar", "4000", "-ac", "1", "-i",
                ])
                .arg(&raw)
                .args(["-c:a", "pcm_f32le"]);
            if tagged {
                encoder.args(["-metadata:s:a:0", "location=private-place-marker"]);
            }
            let encoded = encoder
                .arg(&media)
                .output()
                .expect("ffmpeg is required for the media command regression");
            assert!(
                encoded.status.success(),
                "{}",
                String::from_utf8_lossy(&encoded.stderr)
            );
            let decoded = audio::decode(&media, 0.0, None, 4000).unwrap();
            assert_eq!(decoded, pcm);

            // The separate detector path formerly used by this command misses the pass.
            let dp = detect::DetectParams::default();
            let (ls, lin) = detect::survey(&decoded, 4000, &dp).unwrap();
            let ordinary = detect::detect(&ls, &dp);
            let original_events: Vec<_> = ordinary
                .iter()
                .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
                .map(|d| {
                    measure::measure_family(
                        &decoded,
                        4000,
                        &lin,
                        d,
                        &ordinary,
                        "original",
                        &measure::MeasureParams::default(),
                    )
                })
                .collect();
            assert!(search::independent_measured_events(&original_events, 0.0).is_empty());

            let expected =
                pipeline::measure_recording(&decoded, 4000, &Log::new(true), None).unwrap();
            let selected = search::independent_measured_events(&expected.events, 0.0);
            assert_eq!(selected.len(), n_aircraft, "{label}");
            if n_aircraft > 0 {
                let best = expected
                    .events
                    .iter()
                    .find(|e| e.id == selected[0].id)
                    .unwrap()
                    .fit
                    .as_ref()
                    .unwrap()
                    .best;
                assert!((best.f0 - 83.0).abs() < 0.3, "{best:?}");
                assert!((best.v - 245.0).abs() < 4.0, "{best:?}");
                assert!((best.d / 700.0 - 1.0).abs() < 0.08, "{best:?}");
                assert!((best.t0r - 19.37).abs() < 0.3, "{best:?}");
            }
            let cli = Cli::try_parse_from([
                "fdop",
                "detect",
                media.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--quiet",
                "--json",
            ])
            .unwrap();
            let Cmd::Detect(d) = cli.cmd else {
                unreachable!()
            };
            let mut stdout = Vec::new();
            assert_eq!(run_detect(d, &mut stdout).unwrap(), 0);
            let audit: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(audit["independent_pass_count"], n_aircraft);
            assert_eq!(
                audit["independent_event_ids"],
                serde_json::json!(selected.iter().map(|pass| &pass.id).collect::<Vec<_>>())
            );
            let spans = audit["measured_pass_spans"].as_array().unwrap();
            assert_eq!(spans.len(), n_aircraft);
            for (span, pass) in spans.iter().zip(&selected) {
                assert_eq!(span["event_id"], pass.id);
                assert!((span["first_trace_s"].as_f64().unwrap() - pass.tr_t[0]).abs() < 1e-9);
                assert!((span["last_trace_s"].as_f64().unwrap() - pass.tr_t.last().unwrap()).abs() < 1e-9);
            }
            assert_eq!(audit["audio_stream"], 0);
            assert!(audit["audio_channel"].is_null());
            assert_eq!(audit["channels_in_stream"], 1);
            assert_eq!(audit["decoded_duration_s"], 30.0);
            assert_eq!(audit["location_tag_detected"], tagged);
            assert_eq!(
                audit["warnings"].as_array().unwrap().iter().any(|w| {
                    w.as_str()
                        .is_some_and(|w| w.contains("carries a location tag"))
                }),
                tagged
            );
            let audit_text = std::str::from_utf8(&stdout).unwrap();
            assert!(!audit_text.contains("private-place-marker"));
            let actual = std::fs::read_to_string(out.join("events.json")).unwrap();
            assert_eq!(
                audit["events"],
                serde_json::from_str::<serde_json::Value>(&actual).unwrap()
            );
            assert_eq!(
                actual,
                serde_json::to_string_pretty(&expected.events).unwrap(),
                "{label}"
            );
            assert!(out.join("survey.png").is_file());
            for event in expected.events.iter().filter(|e| e.fit.is_some()) {
                assert!(out.join(format!("{}.png", event.id)).is_file());
                assert!(out.join(format!("{}_trace.csv", event.id)).is_file());
            }
        }

        // Reusing the positive recording's directory for a different recording would
        // leave old event figures and traces beside the new recording's event list.
        let occupied = dir.join("fast");
        let before = std::fs::read(occupied.join("events.json")).unwrap();
        let second_media = dir.join("noise.mka");
        let cli = Cli::try_parse_from([
            "fdop", "detect", second_media.to_str().unwrap(),
            "-o", occupied.to_str().unwrap(), "--json",
        ]).unwrap();
        let Cmd::Detect(d) = cli.cmd else { unreachable!() };
        let mut output = Vec::new();
        assert!(run_detect(d, &mut output).unwrap_err().contains("occupied"));
        assert!(output.is_empty());
        assert_eq!(std::fs::read(occupied.join("events.json")).unwrap(), before);

        // The first track is the negative control. The second contains the known pass
        // on both channels with opposite polarity, so it disappears in a mono mix.
        let multi = dir.join("multi.mka");
        let encoded = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(dir.join("noise.mka"))
            .arg("-i")
            .arg(dir.join("fast.wav"))
            .args([
                "-filter_complex",
                "[1:a]pan=stereo|c0=c0|c1=-1*c0[st]",
                "-map",
                "0:a:0",
                "-map",
                "[st]",
                "-c:a",
                "pcm_f32le",
            ])
            .arg(&multi)
            .output()
            .unwrap();
        assert!(
            encoded.status.success(),
            "{}",
            String::from_utf8_lossy(&encoded.stderr)
        );
        let output = dir.join("whole-file");
        let cli = Cli::try_parse_from([
            "fdop",
            "detect",
            multi.to_str().unwrap(),
            "--offline",
            "--all-audio",
            "--json",
            "--quiet",
            "-o",
            output.to_str().unwrap(),
        ])
        .unwrap();
        let Cmd::Detect(opts) = cli.cmd else {
            unreachable!()
        };
        let mut stdout = Vec::new();
        assert_eq!(run_detect(opts, &mut stdout).unwrap(), 0);
        let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(report["audio_stream_count"], 2);
        assert!(report.get("independent_pass_count").is_none());
        let views = report["sources"].as_array().unwrap();
        assert_eq!(views.len(), 4);
        let tracks = report["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0], serde_json::json!({"audio_stream": 0, "measured_ranges": []}));
        assert_eq!(tracks[1]["audio_stream"], 1);
        let ranges = tracks[1]["measured_ranges"].as_array().unwrap();
        assert_eq!(ranges.len(), 1, "one shared interval across opposite-polarity channels");
        let observations = |k: &str| views[2..].iter()
            .map(|view| view["measured_pass_spans"][0][k].as_f64().unwrap())
            .collect::<Vec<_>>();
        let first = observations("first_trace_s").into_iter().fold(f64::INFINITY, f64::min);
        let last = observations("last_trace_s").into_iter().fold(0.0, f64::max);
        assert!((ranges[0]["start_s"].as_f64().unwrap() - first).abs() < 1e-9);
        assert!((ranges[0]["end_s"].as_f64().unwrap() - last - 1.0 / 4000.0).abs() < 1e-9);
        for (view, (stream, channel, passes, view_dir)) in views.iter().zip([
            (0, None, 0, "stream-0-mix"),
            (1, None, 0, "stream-1-mix"),
            (1, Some(0), 1, "stream-1-channel-0"),
            (1, Some(1), 1, "stream-1-channel-1"),
        ]) {
            assert_eq!(view["audio_stream"], stream);
            assert_eq!(view["audio_channel"], serde_json::json!(channel));
            assert_eq!(view["independent_pass_count"], passes);
            let spans = view["measured_pass_spans"].as_array().unwrap();
            assert_eq!(spans.len(), passes as usize);
            for span in spans {
                assert_eq!(span["event_id"], view["independent_event_ids"][0]);
                assert!(span["last_trace_s"].as_f64().unwrap() > span["first_trace_s"].as_f64().unwrap());
            }
            let events_path = output.join(view_dir).join("events.json");
            assert_eq!(view["events_file"], events_path.to_str().unwrap());
            assert!(events_path.is_file());
        }
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn whole_file_audit_covers_a_stereo_interval_between_mono_sections() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let mut bytes = Vec::new();
        let mono = "anullsrc=r=48000:cl=mono";
        let stereo = "aevalsrc=0.25*sin(2*PI*220*t)|-0.25*sin(2*PI*220*t):s=48000";
        for (index, source) in [mono, stereo, mono].iter().enumerate() {
            let part = dir.path().join(format!("part-{index}.ts"));
            let encoded = Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", source])
                .args(["-frames:a", "500", "-c:a", "mp2", "-threads", "1", "-b:a", "384k"])
                .args(["-mpegts_flags", "+initial_discontinuity", "-muxdelay", "0"])
                .arg("-output_ts_offset")
                .arg((12 + 12 * index).to_string())
                .arg(&part)
                .output().unwrap();
            assert!(encoded.status.success(), "{}", String::from_utf8_lossy(&encoded.stderr));
            bytes.extend(std::fs::read(part).unwrap());
        }
        let path = dir.path().join("changing.ts");
        std::fs::write(&path, bytes).unwrap();
        let (probe, inventory) = audio::AudioProbe::for_offline_audit(&path).unwrap();
        assert_eq!(inventory.streams[0].channels, 1, "stream descriptor misses the stereo interval");
        let mut length = None;
        for channel in [None, Some(0), Some(1)] {
            let selection = AudioSelection { stream: 0, channel };
            let pcm = audio::decode_probed(&probe, 0.0, None, 4000, selection).unwrap();
            assert_eq!(pcm.len(), *length.get_or_insert(pcm.len()));
            let peak = |range: std::ops::Range<usize>| pcm[range].iter()
                .map(|x| x.abs()).fold(0.0_f32, f32::max);
            assert!(peak(8_000..40_000) < 0.001 && peak(104_000..120_000) < 0.001);
            if channel.is_some() {
                assert!(peak(60_000..80_000) > 0.2);
            } else {
                assert!(peak(60_000..80_000) < 0.01, "antiphase audio should cancel in the mix");
            }
        }

        let output = dir.path().join("audit");
        let args = ["fdop", "detect", path.to_str().unwrap(), "--all-audio", "--offline",
                    "--quiet", "--json", "-o", output.to_str().unwrap()];
        let Cmd::Detect(opts) = Cli::try_parse_from(args).unwrap().cmd else { unreachable!() };
        let mut stdout = Vec::new();
        assert_eq!(run_detect(opts, &mut stdout).unwrap(), 0);
        let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        let views = report["sources"].as_array().unwrap();
        assert_eq!(views.len(), 3);
        for (view, channel) in views.iter().zip([None, Some(0), Some(1)]) {
            assert_eq!(view["audio_channel"], serde_json::json!(channel));
            assert_eq!(view["channels_in_stream"], 2);
            assert_eq!(view["decoded_duration_s"], length.unwrap() as f64 / 4000.0);
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn whole_file_audit_checks_tags_after_audio_removal() {
        let root = std::env::temp_dir().join(format!(
            "fdop-muted-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let run = |media: &std::path::Path, json: bool, out: &std::path::Path| {
            let mut args = vec!["fdop", "detect", media.to_str().unwrap(), "--all-audio", "--quiet",
                "-o", out.to_str().unwrap()];
            if json { args.push("--json"); }
            let Cmd::Detect(opts) = Cli::try_parse_from(args).unwrap().cmd else { unreachable!() };
            let mut stdout = Vec::new();
            let status = run_detect(opts, &mut stdout);
            (status, String::from_utf8(stdout).unwrap())
        };
        for tagged in [false, true] {
            let media = root.join(format!("{tagged}.mkv"));
            let mut cmd = std::process::Command::new("ffmpeg");
            cmd.args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=s=16x16:d=0.2", "-an", "-c:v", "ffv1"]);
            if tagged { cmd.args(["-metadata:s:v:0", "location=private-place-marker"]); }
            assert!(cmd.arg(&media).status().unwrap().success());
            let (status, stdout) = run(&media, true, &root.join(format!("json-{tagged}")));
            assert_eq!(status.unwrap(), 0);
            let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(report["audio_stream_count"], 0);
            assert_eq!(report["possible_location_telemetry"], serde_json::json!([]));
            assert_eq!(report["sources"], serde_json::json!([]));
            assert_eq!(report["tracks"], serde_json::json!([]));
            assert_eq!(report["location_tag_detected"], tagged);
            assert!(!stdout.contains("private-place-marker"));
            let (status, stdout) = run(&media, false, &root.join(format!("text-{tagged}")));
            assert_eq!(status.unwrap(), 0);
            let expected = if tagged { "recognized location tag detected" } else { "no recognized location tag detected" };
            assert_eq!(stdout.trim(), format!("no audio streams; {expected}"));
            assert!(audio::probe(&media).is_err());
        }
        let bad = root.join("broken.mkv");
        std::fs::write(&bad, b"not a media container").unwrap();
        let (status, stdout) = run(&bad, true, &root.join("failed"));
        assert!(status.is_err());
        assert!(stdout.is_empty());
    }

    #[test]
    fn measured_ranges_merge_channel_observations_on_the_recording_clock() {
        fn summary(spans: &[(f64, f64)]) -> AudioAuditSummary {
            AudioAuditSummary {
                audio_stream: 0,
                audio_channel: None,
                channels_in_stream: 1,
                decoded_duration_s: 10.0,
                location_tag_detected: false,
                independent_pass_count: spans.len(),
                independent_event_ids: vec![],
                measured_pass_spans: spans.iter().map(|&(first_trace_s, last_trace_s)| MeasuredPassSpan {
                    event_id: "e1".into(), first_trace_s, last_trace_s,
                }).collect(),
                warnings: vec![],
            }
        }
        let views = [summary(&[]), summary(&[(6.5, 9.9), (0.5, 1.0)]), summary(&[(2.0, 3.0)])];
        let ranges = measured_ranges(views.iter(), 1.0).unwrap();
        assert_eq!(ranges, [
            MeasuredRange { start_s: 0.0, end_s: 3.0 + 1.0 / 4000.0 + 1.0 },
            MeasuredRange { start_s: 5.5, end_s: 10.0 },
        ]);
        let one = [summary(&[(2.0, 2.0)])];
        let range = measured_ranges(one.iter(), 0.0).unwrap();
        assert_eq!(range, [MeasuredRange { start_s: 2.0, end_s: 2.0 + 1.0 / 4000.0 }]);
        for padding in [-0.01, f64::NAN, f64::INFINITY] {
            assert!(measured_ranges(one.iter(), padding).is_err());
        }
        for invalid in [(0.2, 0.1), (0.1, f64::NAN), (8.0, 11.0)] {
            assert!(measured_ranges([summary(&[invalid])].iter(), 0.0).is_err());
        }
        let mut incomplete = summary(&[]);
        incomplete.independent_pass_count = 1;
        assert!(measured_ranges([incomplete].iter(), 1.0).is_err());
    }

    #[test]
    fn selective_muting_accepts_multiple_intervals_and_excludes_replacement_audio() {
        let mut args = vec![
            "fdop", "mute", "source.mov", "-o", "edited.mp4",
            "--silence-range", "0..2.5", "--silence-range", "7..10",
        ];
        let Cmd::Mute { silence_range, .. } = Cli::try_parse_from(&args).unwrap().cmd else {
            panic!("expected mute command");
        };
        assert_eq!(silence_range.len(), 2);
        args.extend(["--audio-stream", "1"]);
        assert!(matches!(
            Cli::try_parse_from(&args).unwrap().cmd,
            Cmd::Mute { audio_stream: Some(1), .. }
        ));
        assert!(Cli::try_parse_from([
            "fdop", "mute", "clip.mov", "-o", "out.mp4", "--audio-stream", "1",
        ]).is_err());
        args.extend(["--soundtrack", "voice.wav"]);
        assert!(Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn whole_file_audit_cannot_select_only_one_audio_source() {
        assert!(Cli::try_parse_from(["fdop", "detect", "clip.mkv", "--all-audio"]).is_ok());
        for flag in ["--audio-channel", "--audio-stream"] {
            assert!(
                Cli::try_parse_from(["fdop", "detect", "clip.mkv", "--all-audio", flag, "0"])
                    .is_err()
            );
        }
    }

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

    /// A run with `--start` and the default tolerance must get enough decoy clocks for the curve
    /// match to judge it. The first default, 180 s, did not.
    #[test]
    fn the_default_clock_tolerance_lets_the_curve_match_judge() {
        let args = [
            "fdop",
            "locate",
            "clip.m4a",
            "--dates",
            "2026-09-08",
            "--near",
            "NY",
            "--start",
            "2026-09-08T15:17:30-04:00",
        ];
        let tol = match Cli::try_parse_from(args).unwrap().cmd {
            Cmd::Locate(l) => l.clock_tol,
            _ => panic!("expected locate"),
        };
        assert_eq!(tol, DEFAULT_CLOCK_TOL_S);
        let p = fdop_core::curvematch::MatchParams::default();
        assert!(
            2.0 * tol < fdop_core::curvematch::max_judged_window_s(&p),
            "a +-{tol} s window is not clear of the {} s the decoys allow",
            fdop_core::curvematch::max_judged_window_s(&p)
        );
    }
}
