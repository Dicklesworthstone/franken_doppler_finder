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
pub fn decode(path: &Path, start: f64, dur: Option<f64>, sr: u32) -> Result<Vec<f32>, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    if start > 0.0 {
        cmd.args(["-ss", &format!("{start:.3}")]);
    }
    if let Some(d) = dur {
        cmd.args(["-t", &format!("{d:.3}")]);
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
        "-f",
        "f32le",
        "-",
    ]);
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
}
