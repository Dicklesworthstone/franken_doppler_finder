//! Media probing (ffprobe) and decoding to mono f32 PCM (ffmpeg).

use serde::Serialize;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

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
    pub format: String,
    pub audio_codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub audio_stream: usize,
    pub audio_channel: Option<usize>,
    /// `creation_time` tag (container, then selected audio stream), verbatim.
    pub creation_time: Option<String>,
    /// Parsed creation time (unix s) when the tag carries a zone (or `Z`).
    pub creation_unix: Option<f64>,
    /// ISO 6709 location tag (e.g. Apple `com.apple.quicktime.location.ISO6709`), verbatim.
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
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe not runnable ({e}); install ffmpeg"))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let j: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe JSON: {e}"))?;
    media_info(path, &j, selection)
}

fn media_info(
    path: &Path,
    j: &serde_json::Value,
    selection: AudioSelection,
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
    info.audio_codec = a["codec_name"].as_str().unwrap_or("").to_string();
    info.sample_rate = a["sample_rate"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    info.channels = a["channels"].as_u64().unwrap_or(0) as u32;
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
    for source in [fmt, a] {
        if let Some(tags) = source["tags"].as_object()
            && let Some(time) = tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("creation_time"))
                .and_then(|(_, v)| v.as_str())
        {
            info.creation_time = Some(time.to_string());
            info.creation_unix = fdop_types::time::parse_iso_utc(time).ok();
            break;
        }
    }
    let mut collect_tags = |v: &serde_json::Value| {
        if let Some(m) = v["tags"].as_object() {
            for (k, val) in m {
                if let Some(s) = val.as_str() {
                    info.tags.push((k.clone(), s.to_string()));
                }
            }
        }
    };
    collect_tags(fmt);
    for s in streams {
        collect_tags(s);
    }
    for (k, v) in &info.tags {
        let kl = k.to_ascii_lowercase();
        if (kl.contains("iso6709") || kl == "location" || kl.ends_with(".location"))
            && info.location_iso6709.is_none()
        {
            info.location_iso6709 = Some(v.clone());
            info.location_latlon = parse_iso6709(v);
        }
    }
    Ok(info)
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
        samples.extend(words.iter().map(|word| f32::from_le_bytes(*word)));
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

fn decode_command(mut cmd: Command) -> Result<Vec<f32>, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg not runnable ({e}); install ffmpeg"))?;
    let stderr = child.stderr.take().expect("piped ffmpeg stderr");
    let errors = match std::thread::Builder::new().spawn(move || read_decoder_errors(stderr)) {
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
    let errors = errors
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
        return Err(format!("ffmpeg decode failed: {errors}"));
    }
    Ok(samples)
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
    // `pan` can silently fill an unavailable input channel with zeroes. Validate the
    // source first so an invalid selection cannot look like a silent recording.
    probe_with_selection(path, selection)?;
    let start = start.max(0.0);
    let start_s = format!("{start:.3}");
    let stream = format!("0:a:{}", selection.stream);
    let channel = selection
        .channel
        .map(|n| format!("pan=mono|c0=c{n},"))
        .unwrap_or_default();
    let filter = format!("{channel}asetpts=PTS-{start_s}/TB,aresample=async=1:first_pts=0");
    let mut cmd = Command::new("ffmpeg");
    // MPEG-TS/HLS otherwise collapse timestamp jumps over ten seconds. Retain them, but
    // remove an arbitrary container start timestamp before interpreting relative seeks.
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-copyts",
        "-start_at_zero",
    ]);
    if start > 0.0 {
        cmd.args(["-ss", &start_s]);
    }
    cmd.arg("-i").arg(path);
    cmd.args([
        "-map",
        &stream,
        "-vn",
        "-ac",
        "1",
        "-ar",
        &sr.to_string(),
        // copyts retains the seek offset. Shift it out before filling initial/internal
        // gaps; async=1 only fills/trims, whereas larger values can stretch the signal.
        "-af",
        &filter,
    ]);
    if let Some(d) = dur {
        // Limit output after gap filling: an input limit can stop before the next packet
        // and return no samples for a requested interval wholly inside a timestamp gap.
        cmd.args(["-t", &format!("{d:.3}")]);
    }
    cmd.args(["-f", "f32le", "-"]);
    decode_command(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let values = [0.0, -0.0, 1.0, -1.0, f32::MIN_POSITIVE, f32::MAX, f32::NAN];
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
        let actual = decode_command(command()).unwrap();
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
        let error = decode_command(invalid).unwrap_err();
        assert!(error.contains("ffmpeg decode failed"), "{error}");
        assert!(error.contains("fdop_filter_that_does_not_exist"), "{error}");
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
    fn selected_audio_supplies_the_probe_and_clock_hint() {
        let mut j = serde_json::json!({
            "format": {"format_name": "matroska", "duration": "0"},
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
        assert_eq!(first.creation_time.as_deref(), Some("2026-09-08T11:00:00Z"));
        let selection = AudioSelection {
            stream: 1,
            channel: Some(1),
        };
        let second = media_info(path, &j, selection).unwrap();
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
