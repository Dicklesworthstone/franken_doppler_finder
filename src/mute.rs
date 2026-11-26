//! Make a silent video copy for an owner who wants to remove the entire acoustic channel.

use fdop_core::audio;
use std::path::Path;
use std::process::Command;

pub(crate) fn export(media: &Path, out: &Path) -> Result<(), String> {
    if !out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    {
        return Err("mute output must be a new .mp4 file".into());
    }
    if out.try_exists().map_err(|e| e.to_string())? {
        return Err(format!("output already exists: {}", out.display()));
    }
    let media = media
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", media.display()))?;
    if !media.is_file() {
        return Err("mute requires a local media file".into());
    }

    // Stage beside the destination so the checked file can be installed without
    // copying it or overwriting a destination created while FFmpeg was running.
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("cannot stage export for {}: {e}", out.display()))?;
    let result = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-xerror",
            "-abort_on",
            "empty_output",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(&media)
        // Uppercase V excludes cover art. Disable automatic metadata, chapter
        // and timecode copying in addition to excluding every other stream.
        .args([
            "-map",
            "0:V:0",
            "-an",
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
            "silent MP4 export failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let audit = audio::probe_audio_inventory(staged.path())?;
    if !audit.single_video_without_chapters || audit.location_tag_detected {
        return Err("export verification requires one non-cover video stream, zero chapters and no recognized location tags".into());
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
        export(&media, &out).unwrap();
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
        assert!(export(&media, &out).is_err());
        assert!(export(&out, &out).is_err());
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
            export(&media, &out).unwrap();
            let checked = audio::probe_audio_inventory(&out).unwrap();
            assert!(checked.single_video_without_chapters);
            assert!(checked.possible_location_telemetry.is_empty());
        }
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
        assert!(export(&media, &out).is_err());
        assert!(!out.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
