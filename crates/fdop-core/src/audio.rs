//! Media probing (ffprobe) and decoding to mono f32 PCM (ffmpeg).

use serde::Serialize;
use std::cell::{Cell, OnceCell};
use std::io::{self, BufRead, Read};
use std::path::Path;
use std::process::{Command, Stdio};

// Dynamic containers can introduce an audio stream after the ordinary probe window.
// Let FFmpeg inspect through EOF where the demuxer permits late streams. Discard
// probe packets so discovery does not buffer a long recording in memory.
const FULL_PROBE_ARGS: [&str; 6] = [
    "-probesize",
    "9223372036854775807",
    "-analyzeduration",
    "9223372036854775807",
    "-fflags",
    "+nobuffer",
];

/// Zero-based audio-stream ordinal and optional channel within that stream. With no
/// channel selected, FFmpeg downmixes the selected stream to mono as before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AudioSelection {
    pub stream: usize,
    pub channel: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MediaInfo {
    pub path: String,
    pub duration_s: f64,
    /// Container PTS origin, in seconds. This is independent of the selected track.
    pub start_time_s: f64,
    pub format: String,
    pub audio_codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub audio_stream: usize,
    pub audio_channel: Option<usize>,
    /// First usable `creation_time` tag (container, then selected audio stream), verbatim.
    /// If neither parses, retain the first tag for diagnostics.
    pub creation_time: Option<String>,
    /// Parsed finite creation time (unix s), using the same UTC convention as `--start`.
    pub creation_unix: Option<f64>,
    /// First parsable coordinate tag, or the first nonblank recognized location value
    /// (which may be a place name) if no tag supplies coordinates.
    pub location_iso6709: Option<String>,
    pub location_latlon: Option<(f64, f64)>,
    /// Every tag found, for the report.
    pub tags: Vec<(String, String)>,
}

pub fn probe(path: &Path) -> Result<MediaInfo, String> {
    probe_with_selection(path, AudioSelection::default())
}

/// Describe the selected audio stream and validate its channel before decoding. The
/// stream ordinal counts audio streams only, matching FFmpeg's `0:a:N` mapping.
pub fn probe_with_selection(path: &Path, selection: AudioSelection) -> Result<MediaInfo, String> {
    AudioProbe::open(path)?.selected_info(selection)
}

#[derive(Clone, Copy)]
enum InputAccess {
    Default,
    Offline,
}

impl InputAccess {
    fn command(self, program: &str) -> Command {
        let mut command = Command::new(program);
        if matches!(self, Self::Offline) {
            // FFmpeg applies this whitelist to nested inputs as well. Local
            // segments, local encrypted media and embedded data remain readable.
            command.args(["-protocol_whitelist", "file,crypto,data"]);
        }
        command
    }
}

/// A file's complete stream discovery and, when decoding begins, its ordinary
/// probe. Every selected view uses the same container epoch and stream ordering.
pub struct AudioProbe<'a> {
    path: &'a Path,
    complete: serde_json::Value,
    ordinary: OnceCell<Option<serde_json::Value>>,
    // Input channel counts observed during whole-file audits.
    audit_widths: Vec<Cell<u32>>,
    access: InputAccess,
}

impl<'a> AudioProbe<'a> {
    fn read(path: &'a Path, require_clean_probe: bool, access: InputAccess) -> Result<Self, String> {
        Ok(Self {
            path,
            complete: probe_document(path, require_clean_probe, true, access)?,
            access,
            ordinary: OnceCell::new(),
            audit_widths: Vec::new(),
        })
    }

    /// Discover all streams, including late tracks in dynamic containers.
    pub fn open(path: &'a Path) -> Result<Self, String> {
        Self::read(path, false, InputAccess::Default)
    }

    /// Permit only local files and embedded data throughout discovery and decoding.
    pub fn open_offline(path: &'a Path) -> Result<Self, String> {
        Self::read(path, false, InputAccess::Offline)
    }

    /// Require clean discovery and a complete inventory for a publication audit.
    pub fn for_audit(path: &'a Path) -> Result<(Self, AudioInventory), String> {
        Self::audit(path, InputAccess::Default)
    }

    /// Perform complete publication-audit discovery with network access disabled.
    pub fn for_offline_audit(path: &'a Path) -> Result<(Self, AudioInventory), String> {
        Self::audit(path, InputAccess::Offline)
    }

    fn audit(path: &'a Path, access: InputAccess) -> Result<(Self, AudioInventory), String> {
        let mut probe = Self::read(path, true, access)?;
        let inventory = audit_inventory(path, &probe.complete)?;
        probe.audit_widths = inventory.streams.iter().map(|s| Cell::new(s.channels)).collect();
        Ok((probe, inventory))
    }

    /// FFmpeg gap/overlap correction for a stream on the container-relative clock.
    pub fn clock_resampler(&self, info: &MediaInfo, output_rate: u32) -> String {
        let compensation = timestamp_tolerance(&self.complete, info, output_rate)
            .map(|t| format!(":min_comp={t}:min_hard_comp={t}"))
            .unwrap_or_default();
        format!("aresample=async=1:first_pts=0{compensation}")
    }

    /// Configure input discovery before FFmpeg opens the file. Rewind a full scan
    /// to recover its discarded packets, but avoid seeking an ordinarily visible
    /// compressed stream at zero so the decoder retains its priming samples.
    pub fn configure_input_seek(&self, cmd: &mut Command, info: &MediaInfo, start: f64) {
        let selection = AudioSelection {
            stream: info.audio_stream,
            channel: None,
        };
        let ordinary = self
            .ordinary
            .get_or_init(|| probe_document(self.path, false, false, self.access).ok());
        let header_channels = self.complete["streams"].as_array()
            .and_then(|s| s.iter().filter(|s| s["codec_type"] == "audio").nth(info.audio_stream))
            .and_then(|s| s["channels"].as_u64()).unwrap_or(0);
        let full_probe = ordinary
            .as_ref()
            .and_then(|doc| media_info(self.path, doc, selection).ok())
            .is_none_or(|quick| {
                quick.audio_codec != info.audio_codec
                    || u64::from(quick.channels) != header_channels
                    || quick.sample_rate != info.sample_rate
                    || quick.start_time_s != info.start_time_s
            });
        if full_probe {
            cmd.args(FULL_PROBE_ARGS);
        }
        if start > 0.0 || full_probe {
            cmd.args(["-ss", &format!("{start:.3}")]);
        }
    }

    pub fn selected_info(&self, selection: AudioSelection) -> Result<MediaInfo, String> {
        media_info_with_width(self.path, &self.complete, selection, self.audit_widths.get(selection.stream).map(Cell::get))
    }
}

/// Probe once so a whole-file audit uses the same stream inventory throughout.
pub fn probe_audio_streams(path: &Path) -> Result<Vec<MediaInfo>, String> {
    let doc = probe_document(path, false, true, InputAccess::Default)?;
    all_stream_info(path, &doc)
}

fn all_stream_info(path: &Path, doc: &serde_json::Value) -> Result<Vec<MediaInfo>, String> {
    let count = doc["streams"].as_array().map_or(0, |streams| {
        streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .count()
    });
    if count == 0 {
        return Err(format!("{} has no audio stream", path.display()));
    }
    (0..count)
        .map(|stream| {
            let info = media_info(
                path,
                doc,
                AudioSelection {
                    stream,
                    channel: None,
                },
            )?;
            if info.channels == 0 {
                return Err(format!("unknown channel count for audio stream {stream}"));
            }
            Ok(info)
        })
        .collect()
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TelemetryTrack {
    /// FFprobe index among all streams in the container.
    pub stream_index: u64,
    pub codec_tag: String,
}

/// Stream inventory and metadata findings for a publication audit.
pub struct AudioInventory {
    pub streams: Vec<MediaInfo>,
    pub location_tag_detected: bool,
    /// Data track formats that can carry GPS. Their packets have not been inspected.
    pub possible_location_telemetry: Vec<TelemetryTrack>,
    /// A complete inventory showed one non-cover video stream and zero chapters.
    pub single_video_without_chapters: bool,
    /// Exactly one non-cover video, one audio stream, no other streams and no chapters.
    pub video_with_soundtrack_without_chapters: bool,
    /// Exactly one audio stream, no other streams (including cover art), and no chapters.
    pub single_audio_without_chapters: bool,
}

pub fn probe_audio_inventory(path: &Path) -> Result<AudioInventory, String> {
    audit_inventory(path, &probe_document(path, true, true, InputAccess::Default)?)
}

fn audit_inventory(path: &Path, doc: &serde_json::Value) -> Result<AudioInventory, String> {
    let streams = doc["streams"]
        .as_array()
        .ok_or("ffprobe returned no stream inventory")?;
    // An unidentified stream or an incomplete list cannot establish that audio is absent.
    if streams.is_empty()
        || doc["format"]["nb_streams"].as_u64() != Some(streams.len() as u64)
        || doc["format"]["format_name"].as_str().is_none_or(str::is_empty)
        || streams.iter().any(|s| !matches!(
            s["codec_type"].as_str(),
            Some("audio" | "video" | "subtitle" | "data" | "attachment")
        ))
    {
        return Err(format!("incomplete ffprobe stream inventory for {}", path.display()));
    }
    let audio = if streams.iter().any(|s| s["codec_type"] == "audio") {
        all_stream_info(path, doc)?
    } else {
        Vec::new()
    };
    let location_tag_detected = media_tags(doc).any(|(k, v)| is_location_tag(k, v));
    let mut possible_location_telemetry = Vec::new();
    for stream in streams {
        if stream["codec_type"] == "data"
            && let Some(tag @ ("gpmd" | "camm")) = stream["codec_tag_string"].as_str()
        {
            possible_location_telemetry.push(TelemetryTrack {
                stream_index: stream["index"]
                    .as_u64()
                    .ok_or("telemetry stream lacks an index")?,
                codec_tag: tag.to_owned(),
            });
        }
    }
    let no_chapters = doc["chapters"].as_array().is_some_and(Vec::is_empty);
    let single_audio_without_chapters = streams.len() == 1 && audio.len() == 1 && no_chapters;
    let video_and_audio_only = streams.len() == audio.len() + 1
        && streams.iter().any(|s|
        s["codec_type"] == "video"
        && s["disposition"]["attached_pic"].as_u64() == Some(0)
        && s["disposition"]["timed_thumbnails"].as_u64() == Some(0))
        && no_chapters;
    let single_video_without_chapters = video_and_audio_only && audio.is_empty();
    let video_with_soundtrack_without_chapters = video_and_audio_only && audio.len() == 1;
    Ok(AudioInventory {
        streams: audio,
        location_tag_detected,
        possible_location_telemetry,
        single_video_without_chapters,
        video_with_soundtrack_without_chapters,
        single_audio_without_chapters,
    })
}

fn probe_document(
    path: &Path,
    require_clean_probe: bool,
    complete: bool,
    access: InputAccess,
) -> Result<serde_json::Value, String> {
    let mut cmd = access.command("ffprobe");
    if complete {
        cmd.args(FULL_PROBE_ARGS);
    }
    let out = cmd
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_entries",
            "chapter_tags:program_tags",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe not runnable ({e}); install ffmpeg"))?;
    if !out.status.success() || (require_clean_probe && !out.stderr.is_empty()) {
        return Err(format!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe JSON: {e}"))
}

fn media_info(
    path: &Path,
    j: &serde_json::Value,
    selection: AudioSelection,
) -> Result<MediaInfo, String> {
    media_info_with_width(path, j, selection, None)
}

fn media_info_with_width(
    path: &Path,
    j: &serde_json::Value,
    selection: AudioSelection,
    observed_width: Option<u32>,
) -> Result<MediaInfo, String> {
    let streams = j["streams"]
        .as_array()
        .ok_or_else(|| format!("{} has no audio stream", path.display()))?;
    let n_audio = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .count();
    let a = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .nth(selection.stream)
        .ok_or_else(|| {
            format!(
                "{} has {n_audio} audio stream(s); --audio-stream {} is out of range (indices start at 0)",
                path.display(),
                selection.stream
            )
        })?;
    let mut info = MediaInfo {
        path: path.display().to_string(),
        audio_stream: selection.stream,
        audio_channel: selection.channel,
        ..Default::default()
    };
    let fmt = &j["format"];
    info.format = fmt["format_name"].as_str().unwrap_or("").to_string();
    info.duration_s = fmt["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    info.start_time_s = fmt["start_time"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|time| time.is_finite())
        .unwrap_or(0.0);
    info.audio_codec = a["codec_name"].as_str().unwrap_or("").to_string();
    info.sample_rate = a["sample_rate"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    info.channels = observed_width.unwrap_or_else(|| a["channels"].as_u64().unwrap_or(0) as u32);
    if let Some(channel) = selection.channel
        && channel >= info.channels as usize
    {
        return Err(format!(
            "audio stream {} has {} channel(s); --audio-channel {channel} is out of range (indices start at 0)",
            selection.stream, info.channels
        ));
    }
    if info.duration_s <= 0.0 {
        info.duration_s = a["duration"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
    }
    // An unrelated track may have been dubbed or exported at a different time. Only
    // the container and the selected audio stream can supply the recording-clock hint.
    // An unusable container tag must not hide a usable selected-stream clock. Keep the
    // first unusable tag for diagnostics when neither source supplies a finite instant.
    for source in [fmt, a] {
        if let Some(tags) = source["tags"].as_object()
            && let Some(time) = tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("creation_time"))
                .and_then(|(_, v)| v.as_str())
        {
            let unix = fdop_types::time::parse_iso_utc(time)
                .ok()
                .filter(|time| time.is_finite());
            if info.creation_time.is_none() || unix.is_some() {
                info.creation_time = Some(time.to_string());
                info.creation_unix = unix;
            }
            if unix.is_some() {
                break;
            }
        }
    }
    info.tags = media_tags(j)
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    for (k, v) in &info.tags {
        if !is_location_tag(k, v) {
            continue;
        }
        if let Some(coords) = parse_iso6709(v) {
            info.location_iso6709 = Some(v.clone());
            info.location_latlon = Some(coords);
            break;
        }
        // Text can identify a place too. Keep its presence for the privacy warning,
        // but continue looking across streams for a usable coordinate tag.
        info.location_iso6709.get_or_insert_with(|| v.clone());
    }
    Ok(info)
}

fn media_tags(doc: &serde_json::Value) -> impl Iterator<Item = (&str, &str)> {
    let sections = ["streams", "chapters", "programs"]
        .into_iter()
        .filter_map(|name| doc[name].as_array())
        .flatten();
    std::iter::once(&doc["format"])
        .chain(sections)
        .filter_map(|source| source["tags"].as_object())
        .flatten()
        .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)))
}

fn is_location_tag(key: &str, value: &str) -> bool {
    let key = key.to_ascii_lowercase();
    // FFmpeg can expose a localized MOV tag as "location-eng".
    let localized = key
        .strip_prefix("location-")
        .is_some_and(|lang| lang.len() == 3 && lang.bytes().all(|b| b.is_ascii_lowercase()));
    !value.trim().is_empty()
        && (key.contains("iso6709")
            || localized
            || key.ends_with(".location")
            || matches!(
                key.as_str(),
                "location"
                    | "com.apple.quicktime.location.name"
                    | "recording_location"
                    | "location_name"
                    | "location_latitude"
                    | "location_longitude"
                    | "gpslatitude"
                    | "gpslongitude"
                    | "gpscoordinates"
                    | "gps_latitude"
                    | "gps_longitude"
            ))
}

/// Parse the leading `±DD.DDDD±DDD.DDDD` of an ISO 6709 string.
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let b = s.trim().as_bytes();
    if b.is_empty() || !(b[0] == b'+' || b[0] == b'-') {
        return None;
    }
    let mut i = 1;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    let lat: f64 = std::str::from_utf8(&b[..i]).ok()?.parse().ok()?;
    let j0 = i;
    if j0 >= b.len() || !(b[j0] == b'+' || b[j0] == b'-') {
        return None;
    }
    let mut j = j0 + 1;
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
        j += 1;
    }
    let lon: f64 = std::str::from_utf8(&b[j0..j]).ok()?.parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// Decode the first audio stream, matching [`probe`], to mono f32 at `sr` Hz.
/// Takes `[start, start+dur)` seconds (whole file when `dur` is None).
/// Preserve elapsed time from the container origin: missing packets become silence and
/// overlapping samples are trimmed, without stretching the tones used for Doppler fitting.
pub fn decode(path: &Path, start: f64, dur: Option<f64>, sr: u32) -> Result<Vec<f32>, String> {
    decode_with_selection(path, start, dur, sr, AudioSelection::default())
}

// Keep the conversion buffer and error output bounded. The final samples remain in memory
// for detection, but decoding must not also retain the entire byte representation of them.
const DECODE_BUFFER_BYTES: usize = 64 * 1024;

fn read_pcm(mut input: impl Read) -> io::Result<Vec<f32>> {
    let mut samples = Vec::new();
    let mut buffer = [0u8; DECODE_BUFFER_BYTES];
    let mut pending = 0;
    loop {
        let read = match input.read(&mut buffer[pending..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let available = pending + read;
        let complete = available / 4 * 4;
        let (words, _) = buffer[..complete].as_chunks::<4>();
        samples.try_reserve(words.len()).map_err(|e| {
            io::Error::new(io::ErrorKind::OutOfMemory, format!("decoded audio: {e}"))
        })?;
        for &word in words {
            let sample = f32::from_le_bytes(word);
            if !sample.is_finite() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("non-finite decoded PCM at sample {}", samples.len()),
                ));
            }
            samples.push(sample);
        }
        buffer.copy_within(complete..available, 0);
        pending = available - complete;
    }
    if pending != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "ffmpeg ended inside a 32-bit PCM sample",
        ));
    }
    Ok(samples)
}

fn read_decoder_errors(mut input: impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(DECODE_BUFFER_BYTES as u64)
        .read_to_end(&mut bytes)?;
    // Continue draining even after retaining enough diagnostics, or a full stderr pipe
    // could stop FFmpeg while the sample reader waits for its next block.
    let omitted = io::copy(&mut input, &mut io::sink())?;
    let mut text = String::from_utf8_lossy(&bytes).trim().to_owned();
    if omitted > 0 {
        text.push_str(&format!(
            "\n[ffmpeg stderr truncated; {omitted} further bytes omitted]"
        ));
    }
    Ok(text)
}

// Drain every record, retaining only the widest decoded frame layout and bounded errors.
fn read_audit_log(input: impl Read) -> io::Result<(String, u32)> {
    let mut input = io::BufReader::new(input);
    let mut line = Vec::new();
    let mut errors = Vec::new();
    let mut width = 0;
    let mut invalid_layout = false;
    loop {
        line.clear();
        if input.by_ref().take(DECODE_BUFFER_BYTES as u64).read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.len() == DECODE_BUFFER_BYTES && line.last() != Some(&b'\n') {
            input.skip_until(b'\n')?;
        }
        let text = String::from_utf8_lossy(&line);
        if text.starts_with("[ashowinfo@fdop_layout @ ") && text.contains("] [info] n:") {
            let count = text.split_ascii_whitespace()
                .find_map(|field| field.strip_prefix("channels:"))
                .and_then(|value| value.parse::<u32>().ok());
            match count {
                Some(n) if n > 0 => width = width.max(n),
                _ => invalid_layout = true,
            }
        } else if ["[error]", "[fatal]", "[panic]"].iter().any(|level| text.contains(level)) {
            let n = (DECODE_BUFFER_BYTES - errors.len()).min(line.len());
            errors.extend_from_slice(&line[..n]);
        }
    }
    if invalid_layout {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unreadable FFmpeg audit channel count"));
    }
    Ok((String::from_utf8_lossy(&errors).trim().to_owned(), width))
}

fn decode_command(mut cmd: Command, observe_layouts: bool) -> Result<(Vec<f32>, u32), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg not runnable ({e}); install ffmpeg"))?;
    let stderr = child.stderr.take().expect("piped ffmpeg stderr");
    let errors = match std::thread::Builder::new().spawn(move || {
        if observe_layouts {
            read_audit_log(stderr)
        } else {
            read_decoder_errors(stderr).map(|errors| (errors, 0))
        }
    }) {
        Ok(reader) => reader,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("ffmpeg error reader could not start: {e}"));
        }
    };
    let samples = read_pcm(child.stdout.take().expect("piped ffmpeg stdout"));
    if samples.is_err() {
        // Reap our decoder even after an input/allocation error; it may otherwise be
        // blocked writing samples that the caller can no longer consume.
        let _ = child.kill();
    }
    let status = child.wait();
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let (errors, width) = errors
        .join()
        .map_err(|_| "ffmpeg error reader panicked".to_string())?
        .map_err(|e| format!("ffmpeg error output: {e}"))?;
    let samples = samples.map_err(|e| {
        if errors.is_empty() {
            format!("ffmpeg PCM output: {e}")
        } else {
            format!("ffmpeg PCM output: {e}; {errors}")
        }
    })?;
    let status = status.map_err(|e| format!("ffmpeg wait failed: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg decode failed ({status}): {errors}"));
    }
    if observe_layouts && width == 0 {
        return Err("FFmpeg reported no decoded audio channel layout for the whole-file audit".into());
    }
    Ok((samples, width))
}

// Timestamp and sample clocks quantize each packet boundary. Keep one tick of each
// before treating a discrepancy as missing/overlapping audio. FFmpeg otherwise waits
// for 100 ms, silently moving later flyovers when a shorter packet is lost. Unknown
// or coarser timing retains that existing behavior rather than trusting false precision.
fn timestamp_tolerance(
    doc: &serde_json::Value,
    info: &MediaInfo,
    output_rate: u32,
) -> Option<f32> {
    if info.sample_rate == 0 || output_rate == 0 {
        return None;
    }
    let stream = doc["streams"]
        .as_array()?
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .nth(info.audio_stream)?;
    let (num, den) = stream["time_base"].as_str()?.split_once('/')?;
    let (num, den) = (num.parse::<u64>().ok()?, den.parse::<u64>().ok()?);
    if num == 0 || den == 0 {
        return None;
    }
    let seconds = num as f64 / den as f64
        + 1.0 / info.sample_rate as f64
        + 1.0 / output_rate as f64;
    // Both FFmpeg options are floats. Round upward so serialization cannot tighten
    // the quantization allowance; never widen the existing 100 ms hard threshold.
    let tolerance = (seconds as f32).next_up();
    (tolerance < 0.1).then_some(tolerance)
}

/// Decode one audio stream, optionally taking one channel instead of downmixing. The
/// selected channel retains its amplitude and container-relative packet timestamps.
pub fn decode_with_selection(
    path: &Path,
    start: f64,
    dur: Option<f64>,
    sr: u32,
    selection: AudioSelection,
) -> Result<Vec<f32>, String> {
    decode_probed(&AudioProbe::open(path)?, start, dur, sr, selection)
}

/// Decode a view using discovery shared by all selections from this file.
pub fn decode_probed(
    probe: &AudioProbe<'_>,
    start: f64,
    dur: Option<f64>,
    sr: u32,
    selection: AudioSelection,
) -> Result<Vec<f32>, String> {
    // `pan` can silently fill an unavailable input channel with zeroes. Validate the
    // source first so an invalid selection cannot look like a silent recording.
    let path = probe.path;
    let info = probe.selected_info(selection)?;
    let clock_filter = probe.clock_resampler(&info, sr);
    let start = start.max(0.0);
    let start_s = format!("{start:.3}");
    let stream = format!("0:a:{}", selection.stream);
    let channel = selection
        .channel
        .map(|n| format!("pan=mono|c0=c{n},"))
        .unwrap_or_default();
    // start_at_zero can rebase a transport stream on the first *selected* packet.
    // Subtract the container epoch explicitly to retain an audio track's true delay.
    // A source rate/layout change reinitializes its filtergraph. Normalize in a
    // separate graph so the clock resampler keeps its state instead of inserting
    // the elapsed recording time as silence again at every format change.
    // Shift timestamps before normalization to preserve compressed-audio priming.
    // Observe one original frame at each format change before downmixing.
    let audit_width = probe.audit_widths.get(selection.stream)
        .filter(|_| selection.channel.is_none() && start == 0.0 && dur.is_none());
    let input = if audit_width.is_some() {
        format!("[{stream}]asplit[fdop_src][fdop_layout];[fdop_layout]aselect='eq(n,0)',ashowinfo@fdop_layout,anullsink;[fdop_src]")
    } else {
        format!("[{stream}]")
    };
    let normalize = format!(
        "{input}{channel}asetpts=PTS-({})/TB-{start_s}/TB,aformat=sample_fmts=flt:sample_rates={sr}:channel_layouts=mono[fdop_normalized]",
        info.start_time_s
    );
    let clock =
        format!("[fdop_normalized]{clock_filter}[fdop_clocked]");
    let mut cmd = probe.access.command("ffmpeg");
    if audit_width.is_some() {
        cmd.env("AV_LOG_FORCE_NOCOLOR", "1");
    }
    // Preserve timestamp jumps, including MPEG-TS/HLS gaps over ten seconds.
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-nostats",
        "-loglevel",
        if audit_width.is_some() { "repeat+level+info" } else { "error" },
        // By default FFmpeg can emit partial PCM and exit successfully after corrupt
        // packets. The sample count is used as the recording duration downstream.
        "-xerror",
        "-copyts",
    ]);
    probe.configure_input_seek(&mut cmd, &info, start);
    cmd.arg("-i").arg(path);
    cmd.args([
        "-filter_complex",
        &normalize,
        // async=1 only fills/trims; larger values can stretch the signal.
        "-filter_complex",
        &clock,
        "-map",
        "[fdop_clocked]",
        "-vn",
        "-ac",
        "1",
        "-ar",
        &sr.to_string(),
    ]);
    if let Some(d) = dur {
        // Limit output after gap filling: an input limit can stop before the next packet
        // and return no samples for a requested interval wholly inside a timestamp gap.
        cmd.args(["-t", &format!("{d:.3}")]);
    }
    cmd.args(["-f", "f32le", "-"]);
    let (samples, width) = decode_command(cmd, audit_width.is_some()).map_err(|error| {
        if error.contains("fdop_normalized")
            && (error.contains("Invalid stream specifier") || error.contains("matches no streams"))
        {
            format!("{error}; install FFmpeg 7.1 or newer for linked audio filtergraphs")
        } else {
            error
        }
    })?;
    if let Some(slot) = audit_width {
        slot.set(width);
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_media_commands_allow_only_local_input_protocols() {
        for program in ["ffprobe", "ffmpeg"] {
            let offline = InputAccess::Offline.command(program);
            let args: Vec<_> = offline.get_args().filter_map(|arg| arg.to_str()).collect();
            assert_eq!(args, ["-protocol_whitelist", "file,crypto,data"]);
            assert_eq!(InputAccess::Default.command(program).get_args().count(), 0);
        }
    }

    #[test]
    fn all_stream_probe_keeps_audio_ordinals_and_requires_known_channels() {
        let mut doc = serde_json::json!({"streams": [
            {"codec_type": "video"},
            {"codec_type": "audio", "channels": 1},
            {"codec_type": "subtitle"},
            {"codec_type": "audio", "channels": 2}
        ]});
        let path = Path::new("recording.mkv");
        let streams = all_stream_info(path, &doc).unwrap();
        assert_eq!(
            streams
                .iter()
                .map(|s| (s.audio_stream, s.channels))
                .collect::<Vec<_>>(),
            [(0, 1), (1, 2)]
        );
        doc["streams"][3]["channels"] = serde_json::Value::Null;
        assert!(
            all_stream_info(path, &doc)
                .unwrap_err()
                .contains("audio stream 1")
        );
        assert!(all_stream_info(path, &serde_json::json!({"streams": []})).is_err());
    }

    #[test]
    fn zero_audio_requires_a_complete_stream_inventory_and_still_checks_tags() {
        let path = Path::new("muted.mp4");
        let mut doc = serde_json::json!({
            "format": {"format_name":"mov", "nb_streams":1},
            "streams": [{"codec_type":"video","tags":{"Location":"Test place"}}]
        });
        let inventory = audit_inventory(path, &doc).unwrap();
        assert!(inventory.streams.is_empty());
        assert!(inventory.location_tag_detected);
        assert!(media_info(path, &doc, AudioSelection::default()).is_err());
        doc["streams"][0]["tags"] = serde_json::json!({});
        assert!(!audit_inventory(path, &doc).unwrap().location_tag_detected);
        for invalid in [
            serde_json::json!({}),
            serde_json::json!({"streams":[]}),
            serde_json::json!({"format":{"format_name":"mov","nb_streams":0},"streams":[]}),
            serde_json::json!({"format":{"format_name":"mov","nb_streams":1},"streams":[]}),
            serde_json::json!({"format":{"format_name":"mov","nb_streams":1},"streams":[{}]}),
            serde_json::json!({"format":{"format_name":"mov","nb_streams":1},"streams":[{"codec_type":"unknown"}]}),
        ] {
            assert!(audit_inventory(path, &invalid).is_err());
        }
    }

    #[test]
    fn silent_export_inventory_requires_one_non_cover_video_and_no_chapters() {
        let path = Path::new("export.mp4");
        let clean = serde_json::json!({
            "format": {"format_name": "mov,mp4", "nb_streams": 1},
            "streams": [{"codec_type": "video", "disposition": {"attached_pic": 0, "timed_thumbnails": 0}}],
            "chapters": [],
        });
        assert!(
            audit_inventory(path, &clean)
                .unwrap()
                .single_video_without_chapters
        );
        for (pointer, value) in [
            ("/streams/0/disposition/attached_pic", serde_json::json!(1)),
            (
                "/streams/0/disposition/timed_thumbnails",
                serde_json::json!(1),
            ),
            ("/streams/0/disposition", serde_json::json!({})),
            ("/chapters", serde_json::json!([{}])),
            ("/chapters", serde_json::Value::Null),
        ] {
            let mut doc = clean.clone();
            *doc.pointer_mut(pointer).unwrap() = value;
            assert!(
                !audit_inventory(path, &doc)
                    .unwrap()
                    .single_video_without_chapters,
                "{pointer}"
            );
        }
        let mut extra = clean;
        extra["streams"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"codec_type": "subtitle"}));
        extra["format"]["nb_streams"] = 2.into();
        assert!(
            !audit_inventory(path, &extra)
                .unwrap()
                .single_video_without_chapters
        );
    }

    #[test]
    fn streamed_pcm_preserves_samples_across_partial_reads() {
        struct Fragments<'a> {
            bytes: &'a [u8],
            limit: usize,
            interrupt: bool,
        }
        impl Read for Fragments<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.interrupt {
                    self.interrupt = false;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.interrupt = true;
                let n = buffer.len().min(self.limit);
                self.bytes.read(&mut buffer[..n])
            }
        }
        let values = [0.0, -0.0, 1.0, -1.0, f32::MIN_POSITIVE, f32::MAX];
        let expected: Vec<u32> = (0..40_001)
            .map(|i| values[i % values.len()].to_bits())
            .collect();
        let bytes: Vec<u8> = expected
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        for limit in [1, 2, 3, 4, 7, 4093, DECODE_BUFFER_BYTES] {
            let actual = read_pcm(Fragments {
                bytes: &bytes,
                limit,
                interrupt: true,
            })
            .unwrap();
            assert_eq!(
                actual
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                expected,
                "read fragment size {limit}"
            );
        }
        assert!(read_pcm(&[][..]).unwrap().is_empty());
        for missing in 1..=3 {
            let err = read_pcm(&bytes[..bytes.len() - missing]).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn nonfinite_pcm_is_a_decode_error_at_the_first_bad_sample() {
        let mut bytes = vec![0; DECODE_BUFFER_BYTES + 8];
        for value in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            bytes[DECODE_BUFFER_BYTES..DECODE_BUFFER_BYTES + 4]
                .copy_from_slice(&value.to_le_bytes());
            let err = read_pcm(bytes.as_slice()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                err.to_string(),
                format!(
                    "non-finite decoded PCM at sample {}",
                    DECODE_BUFFER_BYTES / 4
                )
            );
        }
    }

    #[test]
    fn audit_log_finds_late_widths_after_large_diagnostics_and_rejects_unknown_layouts() {
        let prefix = "[ashowinfo@fdop_layout @ 0x1] [info] n:0 pts:0";
        let mut log = format!("{prefix} channels:1\n[error] {}\n", "x".repeat(DECODE_BUFFER_BYTES * 2));
        log.push_str(&format!("{prefix} channels:6\n{prefix} channels:2\n"));
        let (errors, width) = read_audit_log(log.as_bytes()).unwrap();
        assert_eq!(width, 6);
        assert_eq!(errors.len(), DECODE_BUFFER_BYTES);
        log.push_str(&format!("{prefix} channels:unknown\n"));
        assert!(read_audit_log(log.as_bytes()).is_err());
    }

    #[test]
    fn decoder_errors_are_bounded_and_fully_drained() {
        let bytes = vec![b'x'; DECODE_BUFFER_BYTES * 3 + 7];
        let mut cursor = io::Cursor::new(&bytes);
        let errors = read_decoder_errors(&mut cursor).unwrap();
        assert_eq!(cursor.position() as usize, bytes.len());
        assert!(errors.starts_with(&"x".repeat(DECODE_BUFFER_BYTES)));
        assert!(errors.ends_with(&format!(
            "[ffmpeg stderr truncated; {} further bytes omitted]",
            bytes.len() - DECODE_BUFFER_BYTES
        )));
        assert!(errors.len() < DECODE_BUFFER_BYTES + 100);
        assert_eq!(
            read_decoder_errors(&b"  real decoder error\n"[..]).unwrap(),
            "real decoder error"
        );
    }

    #[test]
    #[ignore = "requires ffmpeg"]
    fn streamed_ffmpeg_matches_buffered_decode_and_reports_failure() {
        let command = || {
            let mut cmd = Command::new("ffmpeg");
            cmd.args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=137:sample_rate=4000:duration=17",
                "-f",
                "f32le",
                "-",
            ]);
            cmd
        };
        let expected = command().output().unwrap();
        assert!(expected.status.success());
        let actual = decode_command(command(), false).unwrap().0;
        assert_eq!(actual.len() * 4, expected.stdout.len());
        assert!(
            actual
                .iter()
                .zip(expected.stdout.as_chunks::<4>().0)
                .all(|(sample, word)| sample.to_le_bytes() == *word)
        );
        let mut invalid = Command::new("ffmpeg");
        invalid.args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "fdop_filter_that_does_not_exist",
            "-f",
            "f32le",
            "-",
        ]);
        let error = decode_command(invalid, false).unwrap_err();
        assert!(error.contains("ffmpeg decode failed"), "{error}");
        assert!(error.contains("fdop_filter_that_does_not_exist"), "{error}");
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn corrupt_recording_is_not_accepted_as_a_shorter_decode() {
        let dir = std::env::temp_dir().join(format!(
            "fdop-corrupt-audio-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let made = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=137:sample_rate=8000:duration=2",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        assert_eq!(decode(&path, 0.0, None, 8000).unwrap().len(), 16000);
        // Keep the header intact and remove 200 complete samples. The decoder can
        // emit a long, word-aligned prefix even though the final packet is corrupt.
        let bytes = std::fs::read(&path).unwrap();
        let damaged = dir.join("truncated.wav");
        std::fs::write(&damaged, &bytes[..bytes.len() - 400]).unwrap();
        assert!(probe(&damaged).is_ok());
        let err = decode(&damaged, 0.0, None, 8000).unwrap_err();
        assert!(err.contains("ffmpeg decode failed"), "{err}");
    }

    #[test]
    fn iso6709() {
        assert_eq!(
            parse_iso6709("+40.8765-073.5250+012.000/"),
            Some((40.8765, -73.525))
        );
        assert_eq!(parse_iso6709("garbage"), None);
    }

    #[test]
    fn location_metadata_is_audited_across_the_whole_container() {
        let path = Path::new("tagged.mkv");
        let mut j = serde_json::json!({
            "format": {"tags": {"location": "  "}},
            "streams": [
                {"codec_type": "audio"},
                {"codec_type": "video", "tags": {"COM.APPLE.QUICKTIME.LOCATION.NAME": "Test place"}},
                {"codec_type": "audio", "tags": {"LOCATION-ENG": "+00.0000+000.0000/"}}
            ]
        });
        let probe = |j: &serde_json::Value| media_info(path, j, AudioSelection::default()).unwrap();
        let info = probe(&j);
        assert_eq!(info.location_latlon, Some((0.0, 0.0)));
        assert_eq!(info.location_iso6709.as_deref(), Some("+00.0000+000.0000/"));

        j["streams"][2]["tags"] = serde_json::json!({});
        let info = probe(&j);
        assert!(info.location_latlon.is_none());
        assert_eq!(info.location_iso6709.as_deref(), Some("Test place"));

        j["streams"][1]["tags"] = serde_json::json!({"location-accuracy": "10"});
        assert!(probe(&j).location_iso6709.is_none());
    }

    #[test]
    fn split_gps_and_place_tags_warn_in_each_metadata_scope() {
        let path = Path::new("tagged.mkv");
        let doc = serde_json::json!({
            "format": {"format_name": "matroska", "nb_streams": 1},
            "streams": [{"codec_type": "audio", "channels": 1}],
            "chapters": [{}], "programs": [{}]
        });
        for (section, name) in [
            ("/format", "GPSCoordinates"),
            ("/streams/0", "RECORDING_LOCATION"),
            ("/chapters/0", "GPSLatitude"),
            ("/programs/0", "GPS_LONGITUDE"),
        ] {
            for (value, expected) in [("private-place-marker", true), ("  ", false)] {
                let mut tagged = doc.clone();
                tagged.pointer_mut(section).unwrap()["tags"] = serde_json::json!({(name): value});
                let inventory = audit_inventory(path, &tagged).unwrap();
                assert_eq!(
                    inventory.location_tag_detected, expected,
                    "{section} {name}"
                );
                assert_eq!(inventory.streams[0].location_iso6709.is_some(), expected);
                assert!(inventory.streams[0].location_latlon.is_none());
            }
        }
        let mut unrelated = doc;
        unrelated["chapters"][0]["tags"] =
            serde_json::json!({"title": "GPSLatitude", "GPSLatitudeRef": "N"});
        assert!(
            !audit_inventory(path, &unrelated)
                .unwrap()
                .location_tag_detected
        );
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn chapter_gps_is_visible_in_a_silent_export() {
        let dir = std::env::temp_dir().join(format!(
            "fdop-chapter-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let metadata = dir.join("chapters.ffmeta");
        let video = dir.join("silent.mkv");
        std::fs::write(
            &metadata,
            b";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=200\nGPSLatitude=private-chapter-marker\n",
        )
        .unwrap();
        let export = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=s=16x16:d=0.2",
            ])
            .args(["-f", "ffmetadata", "-i"])
            .arg(metadata)
            .args(["-map", "0:v", "-map_chapters", "1", "-c:v", "ffv1"])
            .arg(&video)
            .output()
            .unwrap();
        assert!(
            export.status.success(),
            "{}",
            String::from_utf8_lossy(&export.stderr)
        );
        let audit = probe_audio_inventory(&video).unwrap();
        assert!(audit.streams.is_empty());
        assert!(audit.location_tag_detected);
    }

    #[test]
    fn selected_audio_supplies_the_probe_and_clock_hint() {
        let mut j = serde_json::json!({
            "format": {"format_name": "matroska", "duration": "0", "start_time": "-12.345678"},
            "streams": [
                {"codec_type": "video", "tags": {"creation_time": "2026-09-07T10:00:00Z"}},
                {"codec_type": "audio", "codec_name": "aac", "sample_rate": "4000",
                 "channels": 1, "duration": "3",
                 "tags": {"creation_time": "2026-09-08T11:00:00Z"}},
                {"codec_type": "subtitle"},
                {"codec_type": "audio", "codec_name": "pcm_f32le", "sample_rate": "8000",
                 "channels": 2, "duration": "4",
                 "tags": {"CREATION_TIME": "2026-09-08T12:00:00Z"}}
            ]
        });
        let path = Path::new("tracks.mkv");
        let first = media_info(path, &j, AudioSelection::default()).unwrap();
        assert_eq!(
            (first.audio_stream, first.sample_rate, first.channels),
            (0, 4000, 1)
        );
        assert_eq!(first.start_time_s, -12.345678);
        assert_eq!(first.creation_time.as_deref(), Some("2026-09-08T11:00:00Z"));
        let selection = AudioSelection {
            stream: 1,
            channel: Some(1),
        };
        let second = media_info(path, &j, selection).unwrap();
        assert_eq!(second.start_time_s, first.start_time_s);
        assert_eq!(second.audio_codec, "pcm_f32le");
        assert_eq!((second.audio_stream, second.audio_channel), (1, Some(1)));
        assert_eq!((second.sample_rate, second.channels), (8000, 2));
        assert_eq!(second.duration_s, 4.0);
        assert_eq!(
            second.creation_time.as_deref(),
            Some("2026-09-08T12:00:00Z")
        );
        assert_eq!(
            second.creation_unix,
            Some(fdop_types::time::parse_iso_utc("2026-09-08T12:00:00Z").unwrap())
        );
        // If the selected stream has no clock, another track's time must not replace it.
        j["streams"][3]["tags"] = serde_json::json!({});
        assert!(
            media_info(path, &j, selection)
                .unwrap()
                .creation_time
                .is_none()
        );
        j["format"]["tags"] = serde_json::json!({"creation_time": "2026-09-08T13:00:00Z"});
        assert_eq!(
            media_info(path, &j, selection)
                .unwrap()
                .creation_time
                .as_deref(),
            Some("2026-09-08T13:00:00Z")
        );
        for bad in [
            AudioSelection {
                stream: 2,
                channel: None,
            },
            AudioSelection {
                stream: 0,
                channel: Some(1),
            },
            AudioSelection {
                stream: 1,
                channel: Some(2),
            },
        ] {
            assert!(
                media_info(path, &j, bad)
                    .unwrap_err()
                    .contains("out of range")
            );
        }
    }

    #[test]
    fn unusable_container_clock_recovers_the_selected_audio_time() {
        let first_time = "2026-09-08T11:00:00Z";
        let second_time = "2026-09-08T08:00:00.125-04:00";
        let unrelated_time = "2026-09-07T10:00:00Z";
        let mut j = serde_json::json!({
            "format": {"tags": {}},
            "streams": [
                {"codec_type": "video", "tags": {"creation_time": unrelated_time}},
                {"codec_type": "audio", "tags": {"creation_time": first_time}},
                {"codec_type": "audio", "tags": {"CREATION_TIME": second_time}}
            ]
        });
        let path = Path::new("clock_fallback.mkv");
        for invalid in [
            "",
            "N/A",
            "0000-00-00T00:00:00Z",
            "2026-02-30T12:00:00Z",
            "NaN",
            "inf",
            "1e309",
        ] {
            j["format"]["tags"] = serde_json::json!({"creation_time": invalid});
            for (stream, time, hour, second) in
                [(0, first_time, 11, 0.0), (1, second_time, 12, 0.125)]
            {
                let info = media_info(
                    path,
                    &j,
                    AudioSelection {
                        stream,
                        channel: None,
                    },
                )
                .unwrap();
                assert_eq!(info.creation_time.as_deref(), Some(time), "{invalid:?}");
                assert_eq!(
                    info.creation_unix,
                    Some(fdop_types::time::unix_from_utc(2026, 9, 8, hour, 0, second)),
                    "container {invalid:?}, audio stream {stream}"
                );
                // Selecting a clock does not erase the original metadata from the report.
                assert_eq!(info.tags.len(), 4);
                for (name, value) in [
                    ("creation_time", invalid),
                    ("creation_time", unrelated_time),
                    ("creation_time", first_time),
                    ("CREATION_TIME", second_time),
                ] {
                    assert!(info.tags.contains(&(name.into(), value.into())));
                }
            }
        }
        // Absent and non-string container tags also leave the selected clock available.
        for tags in [
            serde_json::json!({}),
            serde_json::json!({"creation_time": null}),
            serde_json::json!({"creation_time": 123}),
        ] {
            j["format"]["tags"] = tags;
            let info = media_info(path, &j, AudioSelection::default()).unwrap();
            assert_eq!(info.creation_time.as_deref(), Some(first_time));
            assert_eq!(
                info.creation_unix,
                Some(fdop_types::time::unix_from_utc(2026, 9, 8, 11, 0, 0.0))
            );
        }
        // A valid container still wins, regardless of which valid audio stream is selected.
        let container_time = "2026-09-08T13:00:00Z";
        j["format"]["tags"] = serde_json::json!({"CrEaTiOn_TiMe": container_time});
        for stream in [0, 1] {
            let info = media_info(
                path,
                &j,
                AudioSelection {
                    stream,
                    channel: None,
                },
            )
            .unwrap();
            assert_eq!(info.creation_time.as_deref(), Some(container_time));
            assert_eq!(
                info.creation_unix,
                Some(fdop_types::time::unix_from_utc(2026, 9, 8, 13, 0, 0.0))
            );
        }
    }

    #[test]
    fn unusable_clock_diagnostics_never_borrow_an_unselected_stream() {
        let mut j = serde_json::json!({
            "format": {"tags": {"creation_time": "bad container time"}},
            "streams": [
                {"codec_type": "video", "tags": {"creation_time": "2026-09-08T10:00:00Z"}},
                {"codec_type": "audio", "tags": {"creation_time": "2026-09-08T11:00:00Z"}},
                {"codec_type": "audio", "tags": {"CREATION_TIME": "bad selected time"}}
            ]
        });
        let path = Path::new("unusable_clocks.mkv");
        let selection = AudioSelection {
            stream: 1,
            channel: None,
        };
        for selected_tags in [
            serde_json::json!({"CREATION_TIME": "bad selected time"}),
            serde_json::json!({"CREATION_TIME": "inf"}),
            serde_json::json!({}),
        ] {
            j["streams"][2]["tags"] = selected_tags;
            let info = media_info(path, &j, selection).unwrap();
            assert_eq!(info.creation_time.as_deref(), Some("bad container time"));
            assert!(info.creation_unix.is_none());
            assert!(
                info.tags
                    .contains(&("creation_time".into(), "bad container time".into()))
            );
        }
        j["format"]["tags"] = serde_json::json!({});
        j["streams"][2]["tags"] = serde_json::json!({"CREATION_TIME": "bad selected time"});
        let info = media_info(path, &j, selection).unwrap();
        assert_eq!(info.creation_time.as_deref(), Some("bad selected time"));
        assert!(info.creation_unix.is_none());

        j["streams"][2]["tags"] = serde_json::json!({});
        let info = media_info(path, &j, selection).unwrap();
        assert!(info.creation_time.is_none());
        assert!(info.creation_unix.is_none());
        assert_eq!(
            info.tags.len(),
            2,
            "unselected clocks remain diagnostic tags"
        );
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn multistream_decode_matches_the_probed_audio() {
        let dir = std::env::temp_dir().join(format!(
            "fdop-audio-stream-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two_tracks.mka");
        // A first, mono ambient track and a preferred stereo alternative. Automatic FFmpeg
        // selection picks the alternative, even though probe() describes the first stream.
        let made = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=1:sample_rate=4000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:duration=1:sample_rate=4000",
                "-map",
                "0:a:0",
                "-map",
                "1:a:0",
                "-ac:a:1",
                "2",
                "-disposition:a:0",
                "0",
                "-disposition:a:1",
                "default",
                "-c:a",
                "pcm_f32le",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg must be installed to run the media integration test");
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        let info = probe(&path).unwrap();
        assert_eq!(info.channels, 1);
        assert_eq!(info.sample_rate, 4000);
        let pcm = decode(&path, 0.0, None, 4000).unwrap();
        assert!(!pcm.is_empty());
        let energy = |samples: &[f32], hz: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (k, &x) in samples.iter().enumerate() {
                let phase = std::f64::consts::TAU * hz * k as f64 / 4000.0;
                re += x as f64 * phase.cos();
                im += x as f64 * phase.sin();
            }
            re * re + im * im
        };
        assert!(
            energy(&pcm, 220.0) > 1000.0 * energy(&pcm, 880.0),
            "decode selected the alternate track instead of the probed first track"
        );
        let selection = AudioSelection {
            stream: 1,
            channel: Some(1),
        };
        let info = probe_with_selection(&path, selection).unwrap();
        assert_eq!(
            (info.audio_stream, info.channels, info.audio_channel),
            (1, 2, Some(1))
        );
        let alternate = decode_with_selection(&path, 0.0, None, 4000, selection).unwrap();
        assert_eq!(alternate.len(), pcm.len());
        assert!(
            energy(&alternate, 880.0) > 1000.0 * energy(&alternate, 220.0),
            "explicit selection failed to recover the alternate track"
        );
        // A single complete inventory and ordinary probe serve all views. A cached
        // ordinary probe can still require full discovery for a particular track.
        let (source, inventory) = AudioProbe::for_audit(&path).unwrap();
        assert_eq!(inventory.streams.len(), 2);
        assert!(source.ordinary.get().is_none());
        assert_eq!(
            decode_probed(&source, 0.0, None, 4000, AudioSelection::default()).unwrap(),
            pcm
        );
        let local = AudioProbe::open_offline(&path).unwrap();
        assert_eq!(
            decode_probed(&local, 0.0, None, 4000, selection).unwrap(),
            alternate
        );
        assert!(matches!(source.ordinary.get(), Some(Some(_))));
        assert_eq!(
            decode_probed(&source, 0.0, None, 4000, selection).unwrap(),
            alternate
        );
        for bad in [
            AudioSelection {
                stream: 2,
                channel: None,
            },
            AudioSelection {
                stream: 1,
                channel: Some(2),
            },
        ] {
            let e = decode_with_selection(&path, 0.0, None, 4000, bad).unwrap_err();
            assert!(e.contains("out of range"), "{e}");
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn selected_channel_recovers_antiphase_audio_with_its_timestamps() {
        const SR: u32 = 8000;
        let dir = std::env::temp_dir().join(format!(
            "fdop-audio-channel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("antiphase_with_gap.mka");
        // The first stream establishes a zero container origin. The second starts one
        // second later and drops packets in [3, 5) on that timeline. Its inverse stereo
        // channels cancel completely when downmixed but each retains the full tone.
        let made = Command::new("ffmpeg")
            .args([
                "-nostdin", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "sine=frequency=110:duration=8:sample_rate=8000",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=6:sample_rate=8000",
                "-map", "0:a:0", "-map", "1:a:0",
                "-filter:a:1", "asetnsamples=n=800:p=0,aselect='lt(n,20)+gte(n,40)',pan=stereo|c0=c0|c1=-1*c0,asetpts=PTS+1/TB",
                "-c:a", "pcm_f32le",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg must be installed to run the media integration test");
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        let selection = AudioSelection {
            stream: 1,
            channel: None,
        };
        let mixed = decode_with_selection(&path, 0.0, None, SR, selection).unwrap();
        assert_eq!(mixed.len(), 7 * SR as usize);
        assert!(mixed.iter().all(|&x| x == 0.0));
        let left = AudioSelection {
            channel: Some(0),
            ..selection
        };
        let right = AudioSelection {
            channel: Some(1),
            ..selection
        };
        let pcm = decode_with_selection(&path, 0.0, None, SR, left).unwrap();
        let inverse = decode_with_selection(&path, 0.0, None, SR, right).unwrap();
        assert_eq!(pcm.len(), mixed.len());
        assert_eq!(inverse.len(), pcm.len());
        assert!(pcm[..SR as usize].iter().all(|&x| x == 0.0));
        assert!(
            pcm[3 * SR as usize..5 * SR as usize]
                .iter()
                .all(|&x| x == 0.0)
        );
        for range in [
            SR as usize..3 * SR as usize,
            5 * SR as usize..7 * SR as usize,
        ] {
            let samples = &pcm[range];
            let rms = (samples.iter().map(|&x| (x as f64).powi(2)).sum::<f64>()
                / samples.len() as f64)
                .sqrt();
            assert!(
                rms > 0.08 && rms < 0.09,
                "selected tone lost its amplitude: {rms}"
            );
        }
        assert!(pcm.iter().zip(&inverse).all(|(&a, &b)| a == -b));
        for (start, duration) in [(0.5, 1.0), (2.5, 3.0), (3.5, 1.0), (5.5, 1.0)] {
            let clip = decode_with_selection(&path, start, Some(duration), SR, left).unwrap();
            let lo = (start * SR as f64) as usize;
            let hi = ((start + duration) * SR as f64) as usize;
            assert_eq!(
                clip,
                pcm[lo..hi],
                "selected channel seek {start} changed timestamps"
            );
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn complete_discovery_preserves_compressed_decoder_priming() {
        let dir = std::env::temp_dir().join(format!(
            "fdop-audio-priming-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (extension, codec, input_rate, output_rate) in [
            ("mp3", "libmp3lame", 8000, 8000),
            ("m4a", "aac", 8000, 8000),
            ("mp3", "libmp3lame", 44_100, 4000),
            ("mp3", "libmp3lame", 48_000, 4000),
            ("m4a", "aac", 44_100, 4000),
            ("m4a", "aac", 48_000, 4000),
            // Matroska rounds packet timestamps to milliseconds even for these
            // sample rates. Quantization must not add/drop uninterrupted samples.
            ("mka", "aac", 44_100, 4000),
            ("mka", "aac", 48_000, 4000),
        ] {
            let path = dir.join(format!("tone-{input_rate}.{extension}"));
            let encoded = Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("sine=frequency=440:sample_rate={input_rate}:duration=6"))
                .args(["-c:a", codec])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                encoded.status.success(),
                "{}",
                String::from_utf8_lossy(&encoded.stderr)
            );
            // The original decoder setup retains the codec's leading priming packets.
            let reference = Command::new("ffmpeg")
                .args([
                    "-nostdin",
                    "-v",
                    "error",
                    "-xerror",
                    "-copyts",
                    "-start_at_zero",
                    "-i",
                ])
                .arg(&path)
                .args([
                    "-map",
                    "0:a:0",
                    "-vn",
                    "-ac",
                    "1",
                    "-ar",
                    &output_rate.to_string(),
                    "-af",
                    "asetpts=PTS,aresample=async=1:first_pts=0",
                    "-f",
                    "f32le",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(
                reference.status.success(),
                "{}",
                String::from_utf8_lossy(&reference.stderr)
            );
            let actual = decode(&path, 0.0, None, output_rate).unwrap();
            assert!(actual.iter().any(|x| x.abs() > 0.05));
            let bytes: Vec<u8> = actual.iter().flat_map(|x| x.to_le_bytes()).collect();
            assert_eq!(
                bytes, reference.stdout,
                "{extension} {input_rate}->{output_rate} priming or timing changed"
            );
        }
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn source_format_changes_preserve_audio_and_recording_clock() {
        let dir = std::env::temp_dir().join(format!(
            "fdop-audio-format-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |path: &Path, rate: u32, channels: u32, frequency: u32, offset: f64| {
            let made = Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("sine=frequency={frequency}:sample_rate={rate}"))
                .args(["-frames:a", "100", "-ac", &channels.to_string()])
                .args(["-c:a", "mp2", "-threads", "1", "-output_ts_offset"])
                .arg(offset.to_string())
                .arg(path)
                .output()
                .unwrap();
            assert!(
                made.status.success(),
                "{}",
                String::from_utf8_lossy(&made.stderr)
            );
        };
        // Derive endpoints from the actual decoded frames, including codec priming
        // and trailing samples; nominal generator duration is not the media clock.
        let span = |path: &Path| {
            let result = Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", "a:0", "-show_frames"])
                .args([
                    "-show_entries",
                    "frame=pts_time,duration_time",
                    "-of",
                    "json",
                ])
                .arg(path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let doc: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            let frames = doc["frames"].as_array().unwrap();
            assert!(frames.len() >= 100);
            let seconds = |frame: &serde_json::Value, key: &str| {
                frame[key].as_str().unwrap().parse::<f64>().unwrap()
            };
            let first = frames.first().unwrap();
            let last = frames.last().unwrap();
            (
                seconds(first, "pts_time"),
                seconds(last, "pts_time") + seconds(last, "duration_time"),
            )
        };
        let tone = |pcm: &[f32], start: f64, duration: f64, hz: f64| {
            let lo = (start * 4000.0).round() as usize;
            let count = (duration * 4000.0).round() as usize;
            let window = &pcm[lo..lo + count];
            let rms =
                (window.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / count as f64).sqrt();
            assert!(rms > 0.04, "tone at {start}s disappeared: RMS {rms}");
            let crossings = window
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count();
            let measured = crossings as f64 / duration;
            assert!(
                (measured - hz).abs() <= 1.0 / duration,
                "tone {hz} became {measured} Hz"
            );
        };
        for (name, first_rate, next_rate, next_channels) in
            [("rate", 44_100, 48_000, 1), ("layout", 48_000, 48_000, 2)]
        {
            let first = dir.join(format!("{name}-first.ts"));
            let next = dir.join(format!("{name}-next.ts"));
            make(&first, first_rate, 1, 400, 123.0);
            make(&next, next_rate, next_channels, 600, 0.0);
            let (origin, first_end) = span(&first);
            let (next_start, _) = span(&next);
            for gap in [-0.04, 0.0, 0.04] {
                let shifted = dir.join(format!("{name}-{gap}-shifted.ts"));
                let moved = Command::new("ffmpeg")
                    .args(["-nostdin", "-v", "error", "-copyts", "-i"])
                    .arg(&next)
                    .args([
                        "-map",
                        "0:a:0",
                        "-c:a",
                        "copy",
                        "-mpegts_copyts",
                        "1",
                        "-muxdelay",
                        "0",
                    ])
                    .args([
                        "-output_ts_offset",
                        &(first_end + gap - next_start).to_string(),
                    ])
                    .arg(&shifted)
                    .output()
                    .unwrap();
                assert!(
                    moved.status.success(),
                    "{}",
                    String::from_utf8_lossy(&moved.stderr)
                );
                let (shifted_start, _) = span(&shifted);
                assert!((shifted_start - first_end - gap).abs() < 2.0 / 90_000.0);
                let path = dir.join(format!("{name}-{gap}.ts"));
                let mut bytes = std::fs::read(&first).unwrap();
                bytes.extend(std::fs::read(&shifted).unwrap());
                std::fs::write(&path, bytes).unwrap();
                let (source_start, source_end) = span(&path);
                assert_eq!(source_start, origin);
                let duration = source_end - source_start;
                let switch = shifted_start - origin;
                // Channel zero exists on both sides, even when the layout changes.
                for channel in [None, Some(0)] {
                    let selection = AudioSelection { stream: 0, channel };
                    let pcm = decode_with_selection(&path, 0.0, None, 4000, selection).unwrap();
                    assert!(
                        (pcm.len() as f64 / 4000.0 - duration).abs() <= 2.0 / 4000.0,
                        "{name}, gap {gap}, channel {channel:?}: {} samples for {duration}s",
                        pcm.len()
                    );
                    let first_sound = pcm.iter().position(|x| x.abs() > 0.02).unwrap();
                    let last_sound = pcm.iter().rposition(|x| x.abs() > 0.02).unwrap();
                    assert!(first_sound as f64 / 4000.0 < 0.03);
                    assert!(duration - last_sound as f64 / 4000.0 < 0.03);
                    tone(&pcm, 0.5, 1.0, 400.0);
                    tone(&pcm, switch + 0.4, 1.0, 600.0);
                    if gap > 0.0 {
                        let lo = ((first_end - origin + 0.008) * 4000.0).ceil() as usize;
                        let hi = ((switch - 0.008) * 4000.0).floor() as usize;
                        assert!(
                            pcm[lo..hi].iter().all(|x| x.abs() < 1e-5),
                            "format change erased the true packet gap"
                        );
                    }
                    let start = ((switch - 0.4) * 1000.0).floor() / 1000.0;
                    let clip =
                        decode_with_selection(&path, start, Some(1.2), 4000, selection).unwrap();
                    assert_eq!(clip.len(), 4800);
                    tone(&clip, switch - start - 0.3, 0.2, 400.0);
                    tone(&clip, switch - start + 0.1, 0.2, 600.0);
                }
            }
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn late_transport_audio_keeps_its_recording_offset() {
        const SR: u32 = 4000;
        let dir = std::env::temp_dir().join(format!(
            "fdop-late-audio-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut transport = Vec::new();
        // Audio appears only in the middle program. The video before and after it
        // also keeps that program outside the ordinary beginning/end probe windows.
        for (index, duration, offset) in [(0, 20, 0), (1, 2, 20), (2, 20, 22)] {
            let part = dir.join(format!("part-{index}.ts"));
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=s=640x360:r=25:d={duration}"));
            if index == 1 {
                cmd.args(["-f", "lavfi", "-i", "sine=frequency=400:duration=2"]);
            }
            cmd.args(["-map", "0:v:0"]);
            if index == 1 {
                cmd.args(["-map", "1:a:0", "-c:a", "mp2"]);
            }
            let result = cmd
                .args([
                    "-c:v",
                    "mpeg2video",
                    "-b:v",
                    "1M",
                    "-mpegts_flags",
                    "+initial_discontinuity",
                    "-tables_version",
                    &index.to_string(),
                    "-output_ts_offset",
                    &offset.to_string(),
                ])
                .arg(&part)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            transport.extend(std::fs::read(part).unwrap());
        }
        let path = dir.join("middle-audio.ts");
        std::fs::write(&path, transport).unwrap();
        let (source, inventory) = AudioProbe::for_audit(&path).unwrap();
        assert_eq!(inventory.streams.len(), 1);
        assert_eq!(inventory.streams[0].audio_stream, 0);
        assert_eq!(inventory.streams[0].channels, 1);

        let pcm = decode_probed(&source, 0.0, None, SR, AudioSelection::default()).unwrap();
        let first_sound = |samples: &[f32]| {
            samples.iter().position(|x| x.abs() > 0.02).unwrap() as f64 / SR as f64
        };
        // MPEG video frame timing and MP2 encoder padding shift the audible onset
        // by less than 100 ms. The twenty seconds before the pass must survive.
        assert!((first_sound(&pcm) - 20.0).abs() < 0.1);
        assert!(pcm[..19 * SR as usize].iter().all(|&x| x == 0.0));
        assert!(pcm.len() > 21 * SR as usize);
        let clip = decode_probed(&source, 19.0, Some(2.0), SR, AudioSelection::default()).unwrap();
        assert_eq!(clip.len(), 2 * SR as usize);
        assert!((first_sound(&clip) - 1.0).abs() < 0.1);
    }

    #[test]
    fn timestamp_compensation_respects_selected_source_precision() {
        let mut doc = serde_json::json!({"streams": [
            {"codec_type":"video", "time_base":"1/25"},
            {"codec_type":"audio", "time_base":"1/1000"},
            {"codec_type":"audio", "time_base":"1/90000"}
        ]});
        let mut info = MediaInfo {
            sample_rate: 44_100,
            ..Default::default()
        };
        let first = timestamp_tolerance(&doc, &info, 4000).unwrap();
        assert!((0.001_272_675_7..0.001_272_676).contains(&(first as f64)));
        info.audio_stream = 1;
        info.sample_rate = 48_000;
        let second = timestamp_tolerance(&doc, &info, 4000).unwrap();
        assert!((0.000_281_944_44..0.000_281_944_5).contains(&(second as f64)));
        assert!(second < first);
        for invalid in [
            "", "N/A", "0/1000", "1/0", "-1/1000", "1/-1000", "1/1000/2",
            "NaN/1", "1/1", "1/10",
        ] {
            doc["streams"][2]["time_base"] = serde_json::json!(invalid);
            assert!(timestamp_tolerance(&doc, &info, 4000).is_none(), "{invalid}");
        }
        doc["streams"][2]["time_base"] = serde_json::Value::Null;
        assert!(timestamp_tolerance(&doc, &info, 4000).is_none());
        doc["streams"][2]["time_base"] = serde_json::json!("1/90000");
        assert!(timestamp_tolerance(&doc, &info, 0).is_none());
        info.sample_rate = 0;
        assert!(timestamp_tolerance(&doc, &info, 4000).is_none());
        info.sample_rate = 48_000;
        info.audio_stream = 2;
        assert!(timestamp_tolerance(&doc, &info, 4000).is_none());
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn short_packet_gaps_and_overlaps_preserve_the_audio_clock() {
        const SR: u32 = 8000;
        let dir = std::env::temp_dir().join(format!(
            "fdop-short-audio-gaps-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, filter: &str| {
            let path = dir.join(name);
            let made = Command::new("ffmpeg")
                .args([
                    "-nostdin", "-v", "error", "-f", "lavfi", "-i",
                    "sine=frequency=137:duration=6:sample_rate=8000",
                    "-af", filter, "-c:a", "pcm_f32le",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
            path
        };
        let reference = make("continuous.mka", "anull");
        let reference = decode(&reference, 0.0, None, SR).unwrap();
        assert_eq!(reference.len(), 6 * SR as usize);
        for (gap_ms, offset) in [(2, 0), (10, 0), (40, 0), (90, 0), (40, 123)] {
            let path = make(
                &format!("gap-{gap_ms}-{offset}.mka"),
                &format!(
                    "asetnsamples=n=8:p=0,aselect='lt(n,2000)+gte(n,{})',asetpts=PTS+{offset}/TB",
                    2000 + gap_ms
                ),
            );
            let lo = 2 * SR as usize;
            let hi = lo + gap_ms * 8;
            let mut expected = reference.clone();
            expected[lo..hi].fill(0.0);
            let actual = decode(&path, 0.0, None, SR).unwrap();
            // This checks every surviving sample as well as the missing interval:
            // restoring time must not stretch or shift the original engine tone.
            assert_eq!(actual, expected, "{gap_ms} ms gap, origin {offset}");
            for (start, duration) in [(1.75, 0.5), (3.0, 1.0)] {
                let clip = decode(&path, start, Some(duration), SR).unwrap();
                let a = (start * SR as f64) as usize;
                let b = ((start + duration) * SR as f64) as usize;
                assert_eq!(clip, expected[a..b], "seek {start}, gap {gap_ms}");
            }
            if gap_ms >= 10 {
                let duration = (gap_ms - 2) as f64 / 1000.0;
                let inside = decode(&path, 2.001, Some(duration), SR).unwrap();
                assert_eq!(inside.len(), (gap_ms - 2) * 8);
                assert!(inside.iter().all(|&x| x == 0.0));
            }
        }
        // Four ten-ms packets overlap the preceding packet. A muxer may clamp their
        // PTS to its previous timestamp; they still must not lengthen the recording.
        let overlap = make(
            "overlap.mka",
            "asetnsamples=n=80:p=0,asetpts=PTS-if(gte(T\\,2)\\,0.04/TB\\,0)",
        );
        let expected: Vec<_> = reference[..16_000]
            .iter()
            .chain(&reference[16_320..])
            .copied()
            .collect();
        let actual = decode(&overlap, 0.0, None, SR).unwrap();
        assert_eq!(actual, expected, "overlapping packets changed later sample times");
        for (start, duration) in [(1.75, 0.5), (3.0, 1.0)] {
            let clip = decode(&overlap, start, Some(duration), SR).unwrap();
            let a = (start * SR as f64) as usize;
            let b = ((start + duration) * SR as f64) as usize;
            assert_eq!(clip, expected[a..b], "overlap seek {start}");
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn decode_preserves_recording_timestamps() {
        const SR: u32 = 8000;
        let dir = std::env::temp_dir().join(format!(
            "fdop-audio-timestamps-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, args: &[&str]| {
            let path = dir.join(name);
            let made = Command::new("ffmpeg")
                .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
                .args(args)
                .arg(&path)
                .output()
                .expect("ffmpeg must be installed to run the media integration test");
            assert!(
                made.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&made.stderr)
            );
            path
        };
        let first_packet_time = |path: &Path| {
            let out = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "a:0",
                    "-read_intervals",
                    "%+#1",
                    "-show_entries",
                    "packet=pts_time",
                    "-of",
                    "json",
                ])
                .arg(path)
                .output()
                .expect("ffprobe must be installed to run the media integration test");
            assert!(out.status.success());
            let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            j["packets"][0]["pts_time"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap()
        };
        let tone = "sine=frequency=440:duration=6:sample_rate=8000";
        let reference = make(
            "continuous.mka",
            &["-f", "lavfi", "-i", tone, "-c:a", "pcm_f32le"],
        );
        let reference = decode(&reference, 0.0, None, SR).unwrap();
        assert_eq!(reference.len(), 6 * SR as usize);

        for (name, offset) in [("gap.mka", 0), ("offset_gap.mka", 123)] {
            // Fixed 100 ms packets make the missing interval exactly [2, 4) seconds.
            // An arbitrary container start must not become leading silence.
            let filter = format!(
                "asetnsamples=n=800:p=0,aselect='lt(n,20)+gte(n,40)',asetpts=PTS+{offset}/TB"
            );
            let path = make(
                name,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    tone,
                    "-af",
                    &filter,
                    "-c:a",
                    "pcm_f32le",
                ],
            );
            assert_eq!(first_packet_time(&path), offset as f64);
            let pcm = decode(&path, 0.0, None, SR).unwrap();
            assert_eq!(
                pcm.len(),
                reference.len(),
                "{name}: timestamp gap collapsed"
            );
            assert!(pcm[16_000..32_000].iter().all(|&x| x == 0.0));
            // Surviving samples are identical: timestamp repair must not stretch the tones.
            assert!(pcm[..16_000] == reference[..16_000]);
            assert!(pcm[32_000..] == reference[32_000..]);
            for (start, dur) in [(1.5, 3.0), (2.5, 2.0), (2.5, 1.0), (4.5, 1.0)] {
                let clip = decode(&path, start, Some(dur), SR).unwrap();
                let lo = (start * SR as f64) as usize;
                let hi = ((start + dur) * SR as f64) as usize;
                assert_eq!(clip.len(), hi - lo, "{name}: seek {start} duration {dur}");
                assert!(
                    clip == pcm[lo..hi],
                    "{name}: seek changed the audio timeline"
                );
            }
        }

        // Audio that starts after the video retains its delay from the container origin.
        let delayed = make(
            "delayed_audio.mkv",
            &[
                "-f",
                "lavfi",
                "-i",
                "color=black:size=16x16:rate=10:duration=6",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=4:sample_rate=8000",
                "-filter:a",
                "asetnsamples=n=800:p=0,asetpts=PTS+2/TB",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "ffv1",
                "-c:a",
                "pcm_f32le",
            ],
        );
        assert_eq!(first_packet_time(&delayed), 2.0);
        let pcm = decode(&delayed, 0.0, None, SR).unwrap();
        assert_eq!(pcm.len(), reference.len());
        assert!(pcm[..16_000].iter().all(|&x| x == 0.0));
        assert!(pcm[16_000..] == reference[..32_000]);

        // MPEG-TS otherwise sanitizes jumps over ten seconds before the resampler sees them.
        // Its nonzero starting PTS must still be normalized, even when keeping the gap.
        let transport = make(
            "transport_gap.ts",
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=20:sample_rate=8000",
                "-af",
                "asetnsamples=n=800:p=0,aselect='lt(n,20)+gte(n,180)',asetpts=PTS+123/TB",
                "-c:a",
                "aac",
                "-mpegts_copyts",
                "1",
            ],
        );
        assert!(first_packet_time(&transport) > 120.0);
        let info = probe(&transport).unwrap();
        assert!((19.0..21.0).contains(&info.duration_s));
        let pcm = decode(&transport, 0.0, None, SR).unwrap();
        assert!((pcm.len() as f64 / SR as f64 - info.duration_s).abs() < 0.001);
        assert!(
            pcm[6 * SR as usize..14 * SR as usize]
                .iter()
                .all(|&x| x == 0.0)
        );
        assert!(pcm[19 * SR as usize..].iter().any(|x| x.abs() > 0.05));
    }
}
