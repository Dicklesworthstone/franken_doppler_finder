//! Remove a recording's original soundtrack, optionally supplying audio from a separate file.

use fdop_core::audio;
use std::path::{Path, PathBuf};
use std::process::Command;

fn local_media(path: &Path) -> Result<PathBuf, String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    if !path.is_file() {
        return Err(format!("expected a local media file: {}", path.display()));
    }
    Ok(path)
}

pub(crate) fn export(media: &Path, out: &Path, soundtrack: Option<&Path>) -> Result<(), String> {
    if !out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    {
        return Err("mute output must be a new .mp4 file".into());
    }
    if out.try_exists().map_err(|e| e.to_string())? {
        return Err(format!("output already exists: {}", out.display()));
    }
    let media = local_media(media)?;
    let soundtrack = soundtrack.map(local_media).transpose()?;
    if let Some(track) = &soundtrack {
        if *track == media {
            return Err("the replacement soundtrack must come from a separate file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let source = media.metadata().map_err(|e| e.to_string())?;
            let audio = track.metadata().map_err(|e| e.to_string())?;
            if (source.dev(), source.ino()) == (audio.dev(), audio.ino()) {
                return Err("the replacement soundtrack must come from a separate file".into());
            }
        }
    }

    // Stage beside the destination so the checked file can be installed without
    // copying it or overwriting a destination created while FFmpeg was running.
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("cannot stage export for {}: {e}", out.display()))?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-abort_on",
        "empty_output_stream",
        "-protocol_whitelist",
        "file",
        "-i",
    ])
    .arg(&media);
    if let Some(track) = &soundtrack {
        cmd.args(["-protocol_whitelist", "file", "-i"]).arg(track);
    }
    // Uppercase V excludes cover art. Explicit input numbers prevent FFmpeg
    // from choosing any camera audio or any picture from the replacement.
    cmd.args(["-map", "0:V:0"]);
    if soundtrack.is_some() {
        // Anchor the replacement's first sample to the export timeline. Silence
        // padding makes the copied video's final packet determine the end in both
        // directions, even when the replacement would have ended much earlier.
        cmd.args([
            "-map",
            "1:a:0",
            "-c:a",
            "aac",
            "-af",
            "asetpts=PTS-STARTPTS,apad",
            "-shortest",
        ]);
    } else {
        cmd.arg("-an");
    }
    let result = cmd
        .args([
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
            "-map_metadata:s",
            "-1",
            "-map_chapters",
            "-1",
            "-write_tmcd",
            "0",
            "-c:v",
            "copy",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
            "-y",
        ])
        .arg(staged.path())
        .output()
        .map_err(|e| format!("ffmpeg not runnable ({e}); install ffmpeg"))?;
    if !result.status.success() || !result.stderr.is_empty() {
        return Err(format!(
            "MP4 export failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let audit = audio::probe_audio_inventory(staged.path())?;
    let expected_layout = if soundtrack.is_some() {
        audit.video_with_soundtrack_without_chapters
    } else {
        audit.single_video_without_chapters
    };
    if !expected_layout || audit.location_tag_detected {
        return Err("export verification failed: unexpected streams, chapters or location tags".into());
    }
    staged
        .persist_noclobber(out)
        .map_err(|e| format!("cannot publish {}: {}", out.display(), e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ffmpeg() -> Command {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-v", "error"]);
        cmd
    }

    fn decoded_video_hash(path: &Path) -> Vec<u8> {
        let result = ffmpeg()
            .arg("-i")
            .arg(path)
            .args(["-map", "0:V:0", "-f", "hash", "-"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn exports_only_primary_video_without_tags_or_chapters() {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("camera.mkv");
        let metadata = root.path().join("chapters.txt");
        std::fs::write(&metadata, ";FFMETADATA1\nlocation=private-format-marker\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=400\ntitle=private-chapter-marker\n").unwrap();
        let result = ffmpeg()
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=32x24:r=10:d=0.4",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=32x24:r=10:d=0.4",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=300:duration=0.4",
                "-f",
                "ffmetadata",
                "-i",
            ])
            .arg(&metadata)
            .args([
                "-map",
                "0:v",
                "-map",
                "1:v",
                "-map",
                "2:a",
                "-map",
                "2:a",
                "-map_metadata",
                "3",
                "-map_chapters",
                "3",
                "-metadata:s:v:0",
                "location=private-stream-marker",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
            ])
            .arg(&media)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let original = audio::probe_audio_inventory(&media).unwrap();
        assert_eq!(original.streams.len(), 2);
        assert!(original.location_tag_detected);
        let out = root.path().join("silent.mp4");
        export(&media, &out, None).unwrap();
        let inspect = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_chapters",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(&out)
            .output()
            .unwrap();
        assert!(inspect.status.success());
        let doc: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
        assert_eq!(doc["streams"].as_array().unwrap().len(), 1);
        assert_eq!(doc["streams"][0]["codec_type"], "video");
        assert_eq!(doc["chapters"].as_array().unwrap().len(), 0);
        assert!(
            !String::from_utf8(inspect.stdout)
                .unwrap()
                .contains("private-")
        );
        assert_eq!(decoded_video_hash(&out), decoded_video_hash(&media));

        // A second export cannot replace a completed output. Using the original
        // MP4 as its own destination also leaves its bytes untouched.
        let bytes = std::fs::read(&out).unwrap();
        assert!(export(&media, &out, None).is_err());
        assert!(export(&out, &out, None).is_err());
        assert_eq!(std::fs::read(&out).unwrap(), bytes);
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn flags_possible_telemetry_in_silent_video_and_checks_the_export() {
        let root = tempfile::tempdir().unwrap();
        let clock = root.path().join("clock.mp4");
        assert!(
            ffmpeg()
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    "color=s=32x24:r=2:d=1",
                    "-c:v",
                    "mpeg4",
                    "-timecode",
                    "00:00:00:00"
                ])
                .arg(&clock)
                .status()
                .unwrap()
                .success()
        );
        let clean_control = audio::probe_audio_inventory(&clock).unwrap();
        assert!(clean_control.possible_location_telemetry.is_empty());
        assert!(!clean_control.single_video_without_chapters); // Extra timecode track.

        // Mark the synthetic data track as metadata, then set its sample format.
        // The harmless timecode sample stays in place: there is no GPS payload.
        let mut bytes = std::fs::read(&clock).unwrap();
        let handler_at = bytes
            .windows(16)
            .position(|w| &w[..4] == b"hdlr" && &w[12..] == b"tmcd")
            .expect("timecode media handler")
            + 12;
        bytes[handler_at..handler_at + 4].copy_from_slice(b"meta");
        let tag_at = bytes
            .windows(20)
            .position(|w| &w[..4] == b"stsd" && &w[16..] == b"tmcd")
            .expect("timecode sample description")
            + 16;
        for tag in ["gpmd", "camm"] {
            bytes[tag_at..tag_at + 4].copy_from_slice(tag.as_bytes());
            let media = root.path().join(format!("{tag}.mp4"));
            std::fs::write(&media, &bytes).unwrap();
            let audit = audio::probe_audio_inventory(&media).unwrap();
            assert!(audit.streams.is_empty());
            assert!(!audit.location_tag_detected);
            assert!(!audit.single_video_without_chapters);
            assert_eq!(
                audit.possible_location_telemetry,
                [audio::TelemetryTrack {
                    stream_index: 1,
                    codec_tag: tag.to_owned()
                }]
            );
            let out = root.path().join(format!("{tag}-muted.mp4"));
            export(&media, &out, None).unwrap();
            let checked = audio::probe_audio_inventory(&out).unwrap();
            assert!(checked.single_video_without_chapters);
            assert!(checked.possible_location_telemetry.is_empty());
        }
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn replacement_audio_preserves_video_timing_and_excludes_other_soundtracks() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("camera.mp4");
        // Video starts half a second after the original audio. The second source
        // track and generated timecode must also disappear from the published copy.
        let camera = ffmpeg()
            .args(["-itsoffset", "0.5", "-f", "lavfi", "-i", "testsrc2=s=32x24:r=10:d=1.5",
                "-f", "lavfi", "-i", "sine=frequency=330:duration=2", "-map", "0:v", "-map", "1:a",
                "-map", "1:a", "-fps_mode", "passthrough", "-c:v", "mpeg4", "-c:a", "aac",
                "-timecode", "00:00:00:00", "-metadata", "location=private-source-marker",
                "-movflags", "use_metadata_tags"]).arg(&media).output().unwrap();
        assert!(camera.status.success(), "{}", String::from_utf8_lossy(&camera.stderr));
        let timing = |path: &Path| {
            let packets = Command::new("ffprobe").args(["-v", "error", "-select_streams", "v:0",
                "-show_packets", "-show_entries", "packet=pts_time,duration_time", "-of", "csv=p=0"])
                .arg(path).output().unwrap();
            assert!(packets.status.success());
            String::from_utf8(packets.stdout).unwrap()
        };
        let original_packets = timing(&media);
        assert!(original_packets.starts_with("0.500000,"));
        assert_eq!(original_packets.lines().count(), 15);
        for (label, duration, padded) in [("short", "0.7", true), ("long", "4", false)] {
            let replacement = dir.path().join(format!("{label}.mka"));
            let soundtrack = ffmpeg().args(["-itsoffset", "0.25", "-f", "lavfi", "-i"])
                .arg(format!("sine=frequency=880:duration={duration}"))
                .args(["-f", "lavfi", "-i", "sine=frequency=1230:duration=4", "-map", "0:a",
                    "-map", "1:a", "-c:a", "pcm_s16le", "-metadata:s:a:0", "location=private-replacement-marker"])
                .arg(&replacement).output().unwrap();
            assert!(soundtrack.status.success(), "{}", String::from_utf8_lossy(&soundtrack.stderr));
            let out = dir.path().join(format!("{label}.mp4"));
            export(&media, &out, Some(&replacement)).unwrap();
            assert_eq!(timing(&out), original_packets);
            assert_eq!(decoded_video_hash(&out), decoded_video_hash(&media));
            let inventory = audio::probe_audio_inventory(&out).unwrap();
            assert!(inventory.video_with_soundtrack_without_chapters);
            assert!(!inventory.single_video_without_chapters);
            assert!(!inventory.location_tag_detected);
            assert_eq!(inventory.streams[0].audio_codec, "aac");
            let decoded = ffmpeg().arg("-i").arg(&out).args(["-map", "0:a:0", "-ac", "1",
                "-ar", "8000", "-f", "f32le", "-"]).output().unwrap();
            assert!(decoded.status.success());
            let pcm: Vec<f64> = decoded.stdout.chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()) as f64).collect();
            assert!((15_760..=16_240).contains(&pcm.len()), "{label}: {}", pcm.len());
            // At 8 kHz these 100 ms windows contain whole periods of all three
            // tones. Measure amplitude at each known frequency independently of phase.
            let amplitude = |start: usize, hz: f64| {
                let (mut sin, mut cos) = (0.0, 0.0);
                for (i, &x) in pcm[start..start + 800].iter().enumerate() {
                    let phase = std::f64::consts::TAU * hz * i as f64 / 8000.0;
                    sin += x * phase.sin();
                    cos += x * phase.cos();
                }
                2.0 * sin.hypot(cos) / 800.0
            };
            assert!(amplitude(800, 880.0) > 0.08, "{label}: missing replacement at start");
            assert!(amplitude(800, 330.0) < 0.002 && amplitude(800, 1230.0) < 0.002);
            if padded {
                assert!(pcm[12_000..12_800].iter().all(|x| x.abs() < 0.001));
            } else {
                assert!(amplitude(12_000, 880.0) > 0.08);
            }
        }
        let refused = dir.path().join("refused.mp4");
        assert!(export(&media, &refused, Some(&media)).is_err());
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias.mp4");
            std::fs::hard_link(&media, &alias).unwrap();
            assert!(export(&media, &refused, Some(&alias)).is_err());
        }
        let silent = dir.path().join("silent.mp4");
        export(&media, &silent, None).unwrap();
        assert!(export(&media, &refused, Some(&silent)).is_err());
        assert!(!refused.exists());
    }

    #[test]
    #[ignore = "requires ffmpeg"]
    fn audio_only_source_cannot_publish_an_empty_video() {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("sound.wav");
        assert!(
            ffmpeg()
                .args(["-f", "lavfi", "-i", "sine=duration=0.1"])
                .arg(&media)
                .status()
                .unwrap()
                .success()
        );
        let out = root.path().join("silent.mp4");
        assert!(export(&media, &out, None).is_err());
        assert!(!out.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
