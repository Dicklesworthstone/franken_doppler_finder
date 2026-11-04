//! Media probing (ffprobe) and decoding to mono f32 PCM (ffmpeg).

use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, Default, Serialize)]
pub struct MediaInfo {
    pub path: String,
    pub duration_s: f64,
    pub format: String,
    pub audio_codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    /// `creation_time` tag (container or stream), verbatim.
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
    let mut info = MediaInfo {
        path: path.display().to_string(),
        ..Default::default()
    };
    let fmt = &j["format"];
    info.format = fmt["format_name"].as_str().unwrap_or("").to_string();
    info.duration_s = fmt["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
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
    if let Some(streams) = j["streams"].as_array() {
        for s in streams {
            collect_tags(s);
        }
        if let Some(a) = streams.iter().find(|s| s["codec_type"] == "audio") {
            info.audio_codec = a["codec_name"].as_str().unwrap_or("").to_string();
            info.sample_rate = a["sample_rate"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            info.channels = a["channels"].as_u64().unwrap_or(0) as u32;
            if info.duration_s <= 0.0 {
                info.duration_s = a["duration"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
            }
        } else {
            return Err(format!("{} has no audio stream", path.display()));
        }
    }
    for (k, v) in &info.tags {
        let kl = k.to_ascii_lowercase();
        if kl == "creation_time" && info.creation_time.is_none() {
            info.creation_time = Some(v.clone());
            info.creation_unix = fdop_types::time::parse_iso_utc(v).ok();
        }
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
    let start = start.max(0.0);
    let start_s = format!("{start:.3}");
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
        "0:a:0",
        "-vn",
        "-ac",
        "1",
        "-ar",
        &sr.to_string(),
        // copyts retains the seek offset. Shift it out before filling initial/internal
        // gaps; async=1 only fills/trims, whereas larger values can stretch the signal.
        "-af",
        &format!("asetpts=PTS-{start_s}/TB,aresample=async=1:first_pts=0"),
    ]);
    if let Some(d) = dur {
        // Limit output after gap filling: an input limit can stop before the next packet
        // and return no samples for a requested interval wholly inside a timestamp gap.
        cmd.args(["-t", &format!("{d:.3}")]);
    }
    cmd.args(["-f", "f32le", "-"]);
    let out = cmd
        .output()
        .map_err(|e| format!("ffmpeg not runnable ({e}); install ffmpeg"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg decode failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let (chunks, _) = out.stdout.as_chunks::<4>();
    Ok(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709() {
        assert_eq!(
            parse_iso6709("+40.8765-073.5250+012.000/"),
            Some((40.8765, -73.525))
        );
        assert_eq!(parse_iso6709("garbage"), None);
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
        let energy = |hz: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (k, &x) in pcm.iter().enumerate() {
                let phase = std::f64::consts::TAU * hz * k as f64 / 4000.0;
                re += x as f64 * phase.cos();
                im += x as f64 * phase.sin();
            }
            re * re + im * im
        };
        assert!(
            energy(220.0) > 1000.0 * energy(880.0),
            "decode selected the alternate track instead of the probed first track"
        );
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
