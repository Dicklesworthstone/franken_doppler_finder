//! Export a video with a cleaned soundtrack or a selectively muted audio recording.

use fdop_core::audio;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::process::{Command, Stdio};
use std::str::FromStr;

fn local_media(path: &Path) -> Result<PathBuf, String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    if !path.is_file() {
        return Err(format!("expected a local media file: {}", path.display()));
    }
    Ok(path)
}

/// Half-open interval in seconds on the same container clock as an audio audit.
#[derive(Clone, Debug)]
pub struct SilenceRange {
    start: f64,
    end: f64,
}

impl FromStr for SilenceRange {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let error = || format!("invalid silence range '{value}': use START..END in seconds with 0 <= START < END");
        let (start, end) = value.split_once("..").ok_or_else(error)?;
        let start = start.parse::<f64>().map_err(|_| error())?;
        let end = end.parse::<f64>().map_err(|_| error())?;
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            return Err(error());
        }
        Ok(Self { start, end })
    }
}

/// Round out to complete samples and merge overlapping or adjacent intervals.
fn sample_intervals(ranges: &[SilenceRange], rate: u32) -> Result<Vec<(u64, u64)>, String> {
    let mut intervals = Vec::with_capacity(ranges.len());
    for range in ranges {
        let start = (range.start * f64::from(rate)).floor();
        let end = (range.end * f64::from(rate)).ceil();
        if rate == 0 || end > (1_u64 << 53) as f64 {
            return Err("silence range exceeds the supported sample clock".into());
        }
        intervals.push((start as u64, end as u64));
    }
    intervals.sort_unstable();
    let mut merged = Vec::<(u64, u64)>::new();
    for (start, end) in intervals {
        if let Some(last) = merged.last_mut() && start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    Ok(merged)
}

// Check the decoded export itself, including every channel, up to the last requested
// sample. Stream through a fixed buffer even for multi-hour recordings.
fn verify_silence(path: &Path, intervals: &[(u64, u64)], channels: u32) -> Result<(), String> {
    let frame_bytes = u64::from(channels) * 4;
    let end = intervals.last().expect("nonempty silence ranges").1;
    let extent = end.checked_mul(frame_bytes).filter(|_| channels != 0)
        .ok_or("invalid audio dimensions for silence verification")?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-nostdin", "-v", "error", "-xerror", "-protocol_whitelist", "file", "-i"])
        .arg(path).args(["-map", "0:a:0", "-af", &format!("atrim=end_sample={end}"), "-f", "s32le", "-"])
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("cannot verify redacted audio: {e}"))?;
    let mut pcm = child.stdout.take().unwrap();
    let mut buffer = [0_u8; 32768];
    let mut offset = 0_u64;
    let mut next = 0;
    let checked = (|| -> Result<(), String> {
        loop {
            let count = pcm.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 { break; }
            let limit = offset + count as u64;
            while let Some(&(start, end)) = intervals.get(next) {
                let start = start * frame_bytes;
                let end = end * frame_bytes;
                if start >= limit { break; }
                let lo = (start.max(offset) - offset) as usize;
                let hi = (end.min(limit) - offset) as usize;
                if buffer[lo..hi].iter().any(|&b| b != 0) {
                    return Err("export verification found sound in a requested silence range".into());
                }
                if end > limit { break; }
                next += 1;
            }
            offset = limit;
        }
        if offset < extent {
            return Err("silence range extends beyond the selected audio timeline".into());
        }
        Ok(())
    })();
    drop(pcm);
    if checked.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    checked?;
    if !status.success() {
        return Err(format!("decoding the redacted audio failed ({status})"));
    }
    Ok(())
}

pub(crate) fn export(
    media: &Path,
    out: &Path,
    soundtrack: Option<&Path>,
    silence_ranges: &[SilenceRange],
    audio_stream: Option<usize>,
) -> Result<(), String> {
    if soundtrack.is_some() && !silence_ranges.is_empty() {
        return Err("choose a replacement soundtrack or silence ranges".into());
    }
    if audio_stream.is_some() && silence_ranges.is_empty() {
        return Err("--audio-stream requires at least one --silence-range".into());
    }
    let extension = out.extension().unwrap_or_default();
    let audio_only = extension.eq_ignore_ascii_case("m4a");
    if audio_only && silence_ranges.is_empty() {
        return Err("an .m4a export needs at least one --silence-range".into());
    }
    if !audio_only && !extension.eq_ignore_ascii_case("mp4") {
        return Err("mute output must be a new .mp4 or .m4a file".into());
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
    let mut redaction = None;
    let mut cmd = Command::new("ffmpeg");
    if !silence_ranges.is_empty() {
        let probe = audio::AudioProbe::open_offline(&media)?;
        let stream = audio_stream.unwrap_or(0);
        let info = probe.selected_info(audio::AudioSelection {
            stream,
            channel: None,
        })?;
        let intervals = sample_intervals(silence_ranges, info.sample_rate)?;
        let clock = probe.clock_resampler(&info, info.sample_rate);
        // Shift every stream by the same epoch; the video is still packet-copied.
        // A mid-stream layout/rate change must not reset the sample index or
        // move the requested cuts. With reinitialization disabled FFmpeg rejects
        // audio frames that no longer match its configured input.
        cmd.args(["-copyts", "-itsoffset", &format!("{:.6}", -info.start_time_s),
            &format!("-reinit_filter:a:{stream}"), "0"]);
        probe.configure_input_seek(&mut cmd, &info, 0.0);
        redaction = Some((info, clock, intervals));
    }
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
    if audio_only {
        cmd.arg("-vn");
    } else {
        cmd.args(["-map", "0:V:0", "-c:v", "copy"]);
    }
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
    } else if let Some((info, clock, intervals)) = &redaction {
        let ranges = intervals.iter().map(|(start, end)| format!("gte(n,{start})*lt(n,{end})"))
            .collect::<Vec<_>>().join("+");
        // ALAC keeps silence exact at a cut, without ringing or encoder delay.
        let silence = format!("{clock},aeval='if({ranges},0,val(ch))':c=same");
        cmd.args(["-map", &format!("0:a:{}", info.audio_stream), "-af", &silence, "-c:a", "alac"]);
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
            "-movflags",
            "+faststart",
            "-f",
            if audio_only { "ipod" } else { "mp4" },
            "-y",
        ])
        .arg(staged.path())
        .output()
        .map_err(|e| format!("ffmpeg not runnable ({e}); install ffmpeg"))?;
    if !result.status.success() || !result.stderr.is_empty() {
        return Err(format!(
            "media export failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let audit = audio::probe_audio_inventory(staged.path())?;
    let expected_layout = if audio_only {
        audit.single_audio_without_chapters
    } else if soundtrack.is_some() || redaction.is_some() {
        audit.video_with_soundtrack_without_chapters
    } else {
        audit.single_video_without_chapters
    };
    if !expected_layout || audit.location_tag_detected {
        return Err("export verification failed: unexpected streams, chapters or location tags".into());
    }
    if let Some((info, _, intervals)) = redaction {
        let audio = &audit.streams[0];
        if audio.audio_codec != "alac" || audio.channels != info.channels || audio.sample_rate != info.sample_rate {
            return Err("export verification found a changed audio format".into());
        }
        verify_silence(staged.path(), &intervals, audio.channels)?;
    }
    staged
        .persist_noclobber(out)
        .map_err(|e| format!("cannot publish {}: {}", out.display(), e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parser_and_outward_rounding() {
        for bad in ["", "0-1", "1..1", "-1..2", "1..inf", "NaN..2", "2..1", "0..1..2"] {
            assert!(bad.parse::<SilenceRange>().is_err(), "{bad}");
        }
        let ranges: Vec<_> = ["1.00001..1.5", "0.25001..0.5", "0.5..1.1", "2..3.1"]
            .map(|s| s.parse().unwrap()).into();
        assert_eq!(sample_intervals(&ranges, 100).unwrap(), [(25, 150), (200, 310)]);
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn silences_requested_samples_on_the_recording_clock_in_every_primary_channel() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("camera.mkv");
        // Container starts at 4 s, primary stereo audio starts 0.5 s later and
        // has a 0.5 s packet gap. An alternate microphone would leak every cut.
        let made = ffmpeg().args(["-f", "lavfi", "-i", "testsrc2=s=32x24:r=10:d=5",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000:duration=3.5",
            "-f", "lavfi", "-i", "sine=frequency=333:sample_rate=8000:duration=5",
            "-map", "0:v", "-map", "1:a", "-map", "2:a",
            "-filter:a:0", "asetnsamples=n=800:p=0,aselect='lt(n,10)+gte(n,15)',pan=stereo|c0=c0|c1=-1*c0,asetpts=PTS+0.5/TB",
            "-c:v", "mpeg4", "-c:a", "pcm_s16le", "-output_ts_offset", "4",
            "-metadata", "location=private-source-marker"])
            .arg(&media).output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let ranges = ["2.650875..2.778", "0.750125..0.875"];
        let out = dir.path().join("redacted.mp4");
        export(&media, &out, None, &ranges.map(|s| s.parse().unwrap()), None).unwrap();
        let inventory = audio::probe_audio_inventory(&out).unwrap();
        assert!(inventory.video_with_soundtrack_without_chapters);
        assert!(!inventory.location_tag_detected);
        assert_eq!(inventory.streams[0].channels, 2);
        assert_eq!(decoded_video_hash(&out), decoded_video_hash(&media));
        for channel in 0..2 {
            let selection = audio::AudioSelection { stream: 0, channel: Some(channel) };
            let mut expected = audio::decode_with_selection(&media, 0.0, None, 8000, selection).unwrap();
            assert_eq!(expected.len(), 32_000);
            assert!(expected[..4_000].iter().all(|&s| s == 0.0));
            assert!(expected[12_000..16_000].iter().all(|&s| s == 0.0));
            for span in [6_001..7_000, 21_207..22_224] {
                assert!(expected[span.clone()].iter().any(|s| s.abs() > 0.1));
                expected[span].fill(0.0);
            }
            let actual = audio::decode_with_selection(&out, 0.0, None, 8000, selection).unwrap();
            assert_eq!(actual.len(), expected.len());
            assert!(actual.iter().zip(expected).all(|(a,b)| (a-b).abs() < 1e-7));
        }
        // An unedited interval has signal. The publication check must notice it
        // even though the stereo pair cancels exactly when downmixed to mono.
        assert!(verify_silence(&out, &[(8_000, 8_800)], 2).is_err());
        // Primary audio ends at 4 s but the video continues to 5 s.
        let refused = dir.path().join("refused.mp4");
        assert!(export(&media, &refused, None, &["3.9..4.1".parse().unwrap()], None).is_err());
        assert!(!refused.exists());
    }

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
    fn exports_only_redacted_primary_audio_from_sound_or_video_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let chapters = dir.path().join("markers.txt");
        std::fs::write(&chapters, ";FFMETADATA1\nlocation=private-source-marker\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=private-chapter-marker\n").unwrap();
        for video in [false, true] {
            let media = dir.path().join(if video { "camera.mkv" } else { "memo.mka" });
            let out = media.with_extension("m4a");
            let mut make = ffmpeg();
            make.args(["-f", "lavfi", "-i",
                "aevalsrc=0.5*sin(2*PI*320*t)|0.5*cos(2*PI*440*t):s=8000:d=1",
                "-f", "lavfi", "-i", "sine=frequency=912:duration=1",
                "-f", "ffmetadata", "-i"]).arg(&chapters);
            if video {
                make.args(["-f", "lavfi", "-i", "color=s=32x24:r=10:d=1", "-map", "3:v", "-c:v", "mpeg4"]);
            }
            let made = make.args(["-map", "0:a", "-map", "1:a", "-c:a", "pcm_s16le",
                "-map_metadata", "2", "-map_chapters", "2", "-metadata:s:a:0",
                "title=private-track-marker"]).arg(&media).output().unwrap();
            assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
            let original = audio::probe_audio_inventory(&media).unwrap();
            assert_eq!(original.streams.len(), 2);
            assert!(original.location_tag_detected);
            assert!(!original.single_audio_without_chapters);

            let cut = ["0.250125..0.375375".parse().unwrap()];
            export(&media, &out, None, &cut, None).unwrap();
            let audit = audio::probe_audio_inventory(&out).unwrap();
            assert!(audit.single_audio_without_chapters);
            assert!(!audit.location_tag_detected);
            assert_eq!(audit.streams[0].audio_codec, "alac");
            assert_eq!(audit.streams[0].channels, 2);
            assert_eq!(audit.streams[0].sample_rate, 8000);
            let metadata = Command::new("ffprobe")
                .args(["-v", "error", "-show_format", "-show_streams", "-show_chapters"])
                .arg(&out).output().unwrap();
            assert!(metadata.status.success());
            assert!(!String::from_utf8_lossy(&metadata.stdout).contains("private-"));
            let samples = |file: &Path| {
                let pcm = ffmpeg().arg("-i").arg(file)
                    .args(["-map", "0:a:0", "-f", "s16le", "-"]).output().unwrap();
                assert!(pcm.status.success());
                pcm.stdout
            };
            // 8 kHz stereo s16: compare every interleaved sample and length,
            // including the samples directly on either side of the cut.
            let mut expected = samples(&media);
            assert_eq!(expected.len(), 8000 * 4);
            let span = &mut expected[2001 * 4..3003 * 4];
            assert!(span.iter().any(|&b| b != 0));
            span.fill(0);
            assert_eq!(samples(&out), expected);
            assert!(export(&media, &out, None, &cut, None).is_err());
        }
        let memo = dir.path().join("memo.mka");
        let refused = dir.path().join("refused.m4a");
        assert!(export(&memo, &refused, None, &[], None).is_err());
        assert!(export(&memo, &refused, Some(&memo), &[], None).is_err());
        assert!(export(&memo, &refused, None, &["0.9..1.1".parse().unwrap()], None).is_err());
        assert!(!refused.exists());
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn mutes_only_the_selected_delayed_microphone_on_the_recording_clock() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("microphones.mkv");
        let made = ffmpeg()
            .args(["-f", "lavfi", "-i", "testsrc2=s=32x24:r=4:d=2",
                "-f", "lavfi", "-i", "sine=frequency=955:sample_rate=16000:duration=2",
                "-itsoffset", "0.5", "-f", "lavfi", "-i",
                "aevalsrc=0.5*sin(2*PI*320*t)|0.5*cos(2*PI*440*t):s=8000:d=1",
                "-map", "0:v", "-map", "1:a", "-map", "2:a",
                "-metadata:s:a:1", "location=private-microphone-marker",
                "-c:v", "mpeg4", "-c:a", "pcm_s16le", "-output_ts_offset", "3"])
            .arg(&media).output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let cut = ["0.750125..0.875375".parse().unwrap()];
        for extension in ["mp4", "m4a"] {
            let out = media.with_extension(extension);
            export(&media, &out, None, &cut, Some(1)).unwrap();
            let audit = audio::probe_audio_inventory(&out).unwrap();
            assert!(!audit.location_tag_detected);
            assert_eq!(audit.streams.len(), 1);
            assert_eq!(audit.streams[0].sample_rate, 8000);
            assert_eq!(audit.streams[0].channels, 2);
            if extension == "mp4" {
                assert!(audit.video_with_soundtrack_without_chapters);
                assert_eq!(decoded_video_hash(&out), decoded_video_hash(&media));
            } else {
                assert!(audit.single_audio_without_chapters);
            }
            for channel in [Some(0), Some(1)] {
                let mut expected = audio::decode_with_selection(&media, 0.0, None, 8000,
                    audio::AudioSelection { stream: 1, channel }).unwrap();
                assert_eq!(expected.len(), 12_000);
                assert!(expected[..4000].iter().all(|&s| s == 0.0));
                assert!(expected[6001..7003].iter().any(|s| s.abs() > 0.1));
                expected[6001..7003].fill(0.0);
                let actual = audio::decode_with_selection(&out, 0.0, None, 8000,
                    audio::AudioSelection { stream: 0, channel }).unwrap();
                assert_eq!(actual, expected);
            }
        }
        let refused = dir.path().join("refused.mp4");
        assert!(export(&media, &refused, None, &cut, Some(2)).is_err());
        assert!(export(&media, &refused, None, &[], Some(0)).is_err());
        assert!(!refused.exists());
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
        export(&media, &out, None, &[], None).unwrap();
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
        assert!(export(&media, &out, None, &[], None).is_err());
        assert!(export(&out, &out, None, &[], None).is_err());
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
            export(&media, &out, None, &[], None).unwrap();
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
            export(&media, &out, Some(&replacement), &[], None).unwrap();
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
        assert!(export(&media, &refused, Some(&media), &[], None).is_err());
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias.mp4");
            std::fs::hard_link(&media, &alias).unwrap();
            assert!(export(&media, &refused, Some(&alias), &[], None).is_err());
        }
        let silent = dir.path().join("silent.mp4");
        export(&media, &silent, None, &[], None).unwrap();
        assert!(export(&media, &refused, Some(&silent), &[], None).is_err());
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
        assert!(export(&media, &out, None, &[], None).is_err());
        assert!(!out.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
