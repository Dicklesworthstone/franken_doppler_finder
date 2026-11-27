//! Choose one observed audio source before any flight or location search.
//!
//! A stream's downmix keeps priority. Only a downmix with fewer than two independent
//! measured aircraft tries individual channels. Whole-file recovery applies the same rule
//! to each audio stream in order. No observations are pooled or ranked by search results.

use crate::pipeline::{AudioMeasurements, Log, measure_recording};
use fdop_core::audio::{self, AudioProbe, AudioSelection, MediaInfo};
use fdop_core::search;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub(crate) struct AudioSourceTrial {
    pub audio_stream: usize,
    pub audio_channel: Option<usize>,
    /// None means decoding or measurement failed, not that the source was silent.
    pub independent_events: Option<usize>,
    pub error: Option<String>,
}

pub(crate) struct SelectedAudio {
    pub pcm: Vec<f32>,
    pub info: MediaInfo,
    pub measurements: AudioMeasurements,
    pub trials: Vec<AudioSourceTrial>,
    pub warnings: Vec<String>,
}

/// Recover aircraft canceled or masked by a selected stream's mono downmix. The caller
/// supplies a whole-file audit probe and its already-decoded downmix, so channels that
/// appear later share the same inventory, recording clock and offline input policy.
///
/// Keep the first source with two independent aircraft. If no source reaches two, replace
/// the incumbent only on a strictly larger independent count; ties keep the earlier view.
/// At most the incumbent and one candidate waveform/measurement set are held at once.
pub(crate) fn recover_channels(
    probe: &AudioProbe<'_>,
    initial: Vec<f32>,
    selection: AudioSelection,
    sr: u32,
    log: &Arc<Log>,
) -> Result<SelectedAudio, String> {
    if selection.channel.is_some() {
        return Err("--auto-channel cannot be combined with --audio-channel".into());
    }
    let info = probe.selected_info(selection)?;
    let measurements = measure_recording(&initial, sr, log, None)?;
    let initial_count = search::independent_measured_events(&measurements.events, 0.0).len();
    let channels = info.channels as usize;
    let mut chosen = SelectedAudio {
        pcm: initial,
        info,
        measurements,
        trials: vec![AudioSourceTrial {
            audio_stream: selection.stream,
            audio_channel: None,
            independent_events: Some(initial_count),
            error: None,
        }],
        warnings: Vec::new(),
    };
    if initial_count >= 2 || channels <= 1 {
        return Ok(chosen);
    }
    log.say(&format!(
        "automatic channel recovery: stream {} downmix has {initial_count} independent measured aircraft; checking {channels} channels",
        selection.stream
    ));
    let mut best_count = initial_count;
    for channel in 0..channels {
        let source = AudioSelection {
            stream: selection.stream,
            channel: Some(channel),
        };
        let candidate = decode_and_measure(probe, source, sr, log);
        let (pcm, info, measurements) = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                let warning = format!(
                    "automatic channel recovery could not assess stream {} channel {channel}: {error}; retained the best completed audio view",
                    selection.stream
                );
                log.say(&warning);
                chosen.warnings.push(warning);
                chosen.trials.push(AudioSourceTrial {
                    audio_stream: selection.stream,
                    audio_channel: Some(channel),
                    independent_events: None,
                    error: Some(error),
                });
                continue;
            }
        };
        let count = search::independent_measured_events(&measurements.events, 0.0).len();
        log.say(&format!(
            "automatic channel recovery: stream {} channel {channel} has {count} independent measured aircraft",
            selection.stream
        ));
        chosen.trials.push(AudioSourceTrial {
            audio_stream: selection.stream,
            audio_channel: Some(channel),
            independent_events: Some(count),
            error: None,
        });
        if count > best_count {
            chosen.pcm = pcm;
            chosen.info = info;
            chosen.measurements = measurements;
            best_count = count;
        }
        if count >= 2 {
            break;
        }
    }
    if let Some(channel) = chosen.info.audio_channel {
        let warning = format!(
            "automatic channel recovery selected stream {} channel {channel} with {best_count} independent measured aircraft, after the downmix supplied {initial_count}; all subsequent searches use this one audio view",
            selection.stream
        );
        log.say(&warning);
        chosen.warnings.push(warning);
    }
    Ok(chosen)
}

fn decode_and_measure(
    probe: &AudioProbe<'_>,
    selection: AudioSelection,
    sr: u32,
    log: &Arc<Log>,
) -> Result<(Vec<f32>, MediaInfo, AudioMeasurements), String> {
    let pcm = audio::decode_probed(probe, 0.0, None, sr, selection)?;
    // A full downmix decode discovers channels that appear later in the recording.
    let info = probe.selected_info(selection)?;
    let measurements = measure_recording(&pcm, sr, log, None)?;
    Ok((pcm, info, measurements))
}

fn queue_recovery_channels(
    sources: &mut VecDeque<AudioSelection>,
    selection: AudioSelection,
    channels: usize,
) {
    if selection.channel.is_none() && channels > 1 {
        // The current stream's channels immediately follow its insufficient or failed
        // downmix. Reversing insertion preserves channel order.
        for channel in (0..channels).rev() {
            sources.push_front(AudioSelection {
                stream: selection.stream,
                channel: Some(channel),
            });
        }
    }
}

/// Assess whole audio streams in their original order before any flight search.
///
/// Each downmix gets the same channel recovery as `--auto-channel`. Keep the first
/// complete source with two independent aircraft, or the earliest source with the largest
/// count if none reaches two. A failed source is recorded separately from a silent one.
/// The chosen waveform, measurements and stream-specific metadata always travel together.
/// At most the global incumbent and one candidate waveform/measurement set are held at once.
pub(crate) fn recover_audio(
    probe: &AudioProbe<'_>,
    streams: usize,
    sr: u32,
    log: &Arc<Log>,
) -> Result<SelectedAudio, String> {
    let mut sources: VecDeque<_> = (0..streams)
        .map(|stream| AudioSelection {
            stream,
            channel: None,
        })
        .collect();
    let mut chosen = None;
    let mut best_count = 0;
    let mut trials = Vec::new();
    let mut warnings = Vec::new();
    while let Some(selection) = sources.pop_front() {
        let channel = selection
            .channel
            .map_or_else(|| "downmix".into(), |channel| format!("channel {channel}"));
        let (pcm, info, measurements) = match decode_and_measure(probe, selection, sr, log) {
            Ok(candidate) => candidate,
            Err(error) => {
                let warning = format!(
                    "automatic audio recovery could not assess stream {} {channel}: {error}",
                    selection.stream
                );
                log.say(&warning);
                warnings.push(warning);
                trials.push(AudioSourceTrial {
                    audio_stream: selection.stream,
                    audio_channel: selection.channel,
                    independent_events: None,
                    error: Some(error),
                });
                // One corrupt channel can poison a mix while another remains usable.
                // Keep the probe's known width, including any completed decode audit.
                if selection.channel.is_none()
                    && let Ok(info) = probe.selected_info(selection)
                {
                    queue_recovery_channels(&mut sources, selection, info.channels as usize);
                }
                continue;
            }
        };
        let channels = info.channels as usize;
        let count = search::independent_measured_events(&measurements.events, 0.0).len();
        log.say(&format!(
            "automatic audio recovery: stream {} {channel} has {count} independent measured aircraft",
            selection.stream
        ));
        trials.push(AudioSourceTrial {
            audio_stream: selection.stream,
            audio_channel: selection.channel,
            independent_events: Some(count),
            error: None,
        });
        if chosen.is_none() || count > best_count {
            chosen = Some(SelectedAudio {
                pcm,
                info,
                measurements,
                trials: Vec::new(),
                warnings: Vec::new(),
            });
            best_count = count;
        }
        if count >= 2 {
            break;
        }
        queue_recovery_channels(&mut sources, selection, channels);
    }
    let mut chosen = chosen.ok_or_else(|| {
        if trials.is_empty() {
            "--auto-audio: the file has no audio streams".to_string()
        } else {
            format!(
                "--auto-audio could not decode and measure any audio stream: {}",
                warnings.join("; ")
            )
        }
    })?;
    chosen.trials = trials;
    chosen.warnings = warnings;
    if chosen.info.audio_stream != 0 || chosen.info.audio_channel.is_some() {
        let channel = chosen
            .info
            .audio_channel
            .map_or_else(|| "mix".into(), |channel| channel.to_string());
        let warning = format!(
            "automatic audio recovery selected stream {} channel {channel} with {best_count} independent measured aircraft; all subsequent searches use this one audio view",
            chosen.info.audio_stream
        );
        log.say(&warning);
        chosen.warnings.push(warning);
    }
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fdop_core::physics;
    use std::io::Write;
    use std::path::Path;

    // These physical waveforms are the compact edge-pass fixtures exercised by
    // pipeline::short_window_recovery_supplies_a_missing_second_aircraft.
    fn recording(second_aircraft: bool) -> Vec<f32> {
        let mut seed = 4242_u64;
        (0..30 * 4000)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / 4000.0;
                let tau = physics::retarded_tau(t, 19.37 - 700.0 / 343.0, 245.0, 700.0, 343.0);
                let range = physics::cv_radial(tau, 245.0, 700.0).0;
                let source = 0.32 * 700.0 / range * (std::f64::consts::TAU * 83.0 * tau).sin();
                let noise = 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0);
                let mut sample =
                    (source + noise + 0.03 * (std::f64::consts::TAU * 250.0 * t).sin()) as f32;
                if second_aircraft {
                    let tau = physics::retarded_tau(t, 9.13 - 900.0 / 343.0, 90.0, 900.0, 343.0);
                    let range = physics::cv_radial(tau, 90.0, 900.0).0;
                    sample +=
                        (0.08 * 900.0 / range * (std::f64::consts::TAU * 450.0 * tau).sin()) as f32;
                }
                sample
            })
            .collect()
    }

    fn stereo_wav(path: &Path, samples: &[f32], antiphase: bool) {
        stereo_wav_other_channel(
            path,
            samples,
            |sample| {
                if antiphase { -sample } else { sample }
            },
        );
    }

    fn stereo_wav_other_channel(path: &Path, samples: &[f32], other: impl Fn(f32) -> f32) {
        // IEEE float WAV preserves the established fixture exactly on channel zero.
        let bytes = u32::try_from(samples.len() * 8).unwrap();
        let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        out.write_all(b"RIFF").unwrap();
        out.write_all(&(36 + bytes).to_le_bytes()).unwrap();
        out.write_all(b"WAVEfmt ").unwrap();
        out.write_all(&16_u32.to_le_bytes()).unwrap();
        out.write_all(&3_u16.to_le_bytes()).unwrap();
        out.write_all(&2_u16.to_le_bytes()).unwrap();
        out.write_all(&4000_u32.to_le_bytes()).unwrap();
        out.write_all(&32000_u32.to_le_bytes()).unwrap();
        out.write_all(&8_u16.to_le_bytes()).unwrap();
        out.write_all(&32_u16.to_le_bytes()).unwrap();
        out.write_all(b"data").unwrap();
        out.write_all(&bytes.to_le_bytes()).unwrap();
        for &sample in samples {
            out.write_all(&sample.to_le_bytes()).unwrap();
            let other_sample = other(sample);
            out.write_all(&other_sample.to_le_bytes()).unwrap();
        }
        out.flush().unwrap();
    }

    fn recover_fixture(path: &Path, samples: &[f32]) -> SelectedAudio {
        stereo_wav(path, samples, true);
        let (probe, _) = AudioProbe::for_offline_audit(path)
            .expect("ffprobe is required by audio-source recovery");
        let selection = AudioSelection::default();
        let initial = audio::decode_probed(&probe, 0.0, None, 4000, selection)
            .expect("ffmpeg is required by audio-source recovery");
        assert_eq!(initial.len(), samples.len());
        assert!(initial.iter().all(|x| x.abs() < 1e-7));
        recover_channels(&probe, initial, selection, 4000, &Log::new(true)).unwrap()
    }

    fn two_track_media(path: &Path, first: &Path, second: &Path, delay_s: u32) {
        let encoded = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(first)
            .arg("-itsoffset")
            .arg(delay_s.to_string())
            .arg("-i")
            .arg(second)
            .args([
                "-map",
                "0:a:0",
                "-map",
                "1:a:0",
                "-map_metadata",
                "-1",
                "-c:a",
                "copy",
                "-metadata:s:a:0",
                "creation_time=2026-09-08T00:00:00Z",
                "-metadata:s:a:1",
                "creation_time=2026-09-08T12:00:00Z",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            encoded.status.success(),
            "{}",
            String::from_utf8_lossy(&encoded.stderr)
        );
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_audio_recovery_restores_a_delayed_antiphase_track() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("silent.wav");
        let second = dir.path().join("aircraft.wav");
        let path = dir.path().join("multitrack.mka");
        let samples = recording(true);
        stereo_wav(&first, &vec![0.0; samples.len()], false);
        stereo_wav(&second, &samples, true);
        two_track_media(&path, &first, &second, 5);
        let (probe, inventory) = AudioProbe::for_offline_audit(&path).unwrap();
        assert_eq!(inventory.streams.len(), 2);
        let result = recover_audio(&probe, inventory.streams.len(), 4000, &Log::new(true)).unwrap();

        assert_eq!(result.info.audio_stream, 1);
        assert_eq!(result.info.audio_channel, Some(0));
        assert_eq!(result.pcm.len(), samples.len() + 20_000);
        assert!(result.pcm[..20_000].iter().all(|&sample| sample == 0.0));
        assert_eq!(&result.pcm[20_000..], samples.as_slice());
        assert_eq!(
            result.info.creation_unix,
            Some(fdop_types::time::parse_iso_utc("2026-09-08T12:00:00Z").unwrap()),
            "the unrelated first stream must not supply the recording clock"
        );
        assert_eq!(
            result
                .trials
                .iter()
                .map(|trial| (
                    trial.audio_stream,
                    trial.audio_channel,
                    trial.independent_events,
                ))
                .collect::<Vec<_>>(),
            [
                (0, None, Some(0)),
                (0, Some(0), Some(0)),
                (0, Some(1), Some(0)),
                (1, None, Some(0)),
                (1, Some(0), Some(2)),
            ]
        );
        assert!(result.trials.iter().all(|trial| trial.error.is_none()));
        let events = search::independent_measured_events(&result.measurements.events, 0.0);
        assert_eq!(events.len(), 2);
        for (tone, heard) in [(83.0, 24.37), (450.0, 14.13)] {
            let event = events
                .iter()
                .find(|event| (event.f0 - tone).abs() < 0.3)
                .expect("both physical aircraft must survive source selection");
            assert!(
                (event.t_rec - heard).abs() < 0.3,
                "track delay must remain on the container recording clock: {event:?}"
            );
        }
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_audio_recovery_preserves_a_usable_first_track() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        let path = dir.path().join("multitrack.mka");
        let samples = recording(true);
        stereo_wav(&first, &samples, false);
        stereo_wav(&second, &samples, true);
        two_track_media(&path, &first, &second, 5);
        let (probe, inventory) = AudioProbe::for_offline_audit(&path).unwrap();
        let expected =
            audio::decode_probed(&probe, 0.0, None, 4000, AudioSelection::default()).unwrap();
        let result = recover_audio(&probe, inventory.streams.len(), 4000, &Log::new(true)).unwrap();
        assert_eq!(result.info.audio_stream, 0);
        assert_eq!(result.info.audio_channel, None);
        assert_eq!(result.pcm, expected);
        assert_eq!(result.pcm.len(), samples.len());
        assert_eq!(
            result.trials.len(),
            1,
            "a complete first source stops selection"
        );
        assert_eq!(result.trials[0].independent_events, Some(2));
        assert!(result.trials[0].error.is_none());
        assert!(result.warnings.is_empty());
        assert_eq!(
            result.info.creation_unix,
            Some(fdop_types::time::parse_iso_utc("2026-09-08T00:00:00Z").unwrap())
        );
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_audio_recovery_never_pools_tracks_or_channels() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        let path = dir.path().join("duplicates.mka");
        let samples = recording(false);
        stereo_wav(&first, &samples, true);
        stereo_wav(&second, &samples, true);
        two_track_media(&path, &first, &second, 0);
        let (probe, inventory) = AudioProbe::for_offline_audit(&path).unwrap();
        let result = recover_audio(&probe, inventory.streams.len(), 4000, &Log::new(true)).unwrap();
        assert_eq!(result.info.audio_stream, 0);
        assert_eq!(
            result.info.audio_channel,
            Some(0),
            "ties keep the first view"
        );
        assert_eq!(result.pcm, samples);
        assert_eq!(
            result
                .trials
                .iter()
                .map(|trial| (
                    trial.audio_stream,
                    trial.audio_channel,
                    trial.independent_events,
                ))
                .collect::<Vec<_>>(),
            [
                (0, None, Some(0)),
                (0, Some(0), Some(1)),
                (0, Some(1), Some(1)),
                (1, None, Some(0)),
                (1, Some(0), Some(1)),
                (1, Some(1), Some(1)),
            ]
        );
        assert!(result.trials.iter().all(|trial| trial.error.is_none()));
        assert_eq!(
            search::independent_measured_events(&result.measurements.events, 0.0).len(),
            1,
            "duplicate views of one pass cannot manufacture a second aircraft"
        );
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_audio_recovery_restores_a_healthy_channel_after_mix_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partly-invalid.wav");
        let samples = recording(true);
        stereo_wav_other_channel(&path, &samples, |_| f32::NAN);
        let (probe, inventory) = AudioProbe::for_offline_audit(&path).unwrap();
        let result = recover_audio(&probe, inventory.streams.len(), 4000, &Log::new(true)).unwrap();
        assert_eq!(result.info.audio_stream, 0);
        assert_eq!(result.info.audio_channel, Some(0));
        assert_eq!(result.pcm, samples);
        assert_eq!(result.trials.len(), 2);
        assert_eq!(result.trials[0].audio_channel, None);
        assert_eq!(result.trials[0].independent_events, None);
        assert!(result.trials[0].error.is_some());
        assert_eq!(result.trials[1].audio_channel, Some(0));
        assert_eq!(result.trials[1].independent_events, Some(2));
        assert!(result.trials[1].error.is_none());
        assert_eq!(
            search::independent_measured_events(&result.measurements.events, 0.0).len(),
            2
        );
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_audio_recovery_skips_a_failed_earlier_track() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("invalid-pcm.wav");
        let second = dir.path().join("aircraft.wav");
        let path = dir.path().join("recoverable.mka");
        let samples = recording(true);
        // The inventory remains valid, but actual decoding must reject non-finite PCM.
        stereo_wav(&first, &vec![f32::NAN; samples.len()], false);
        stereo_wav(&second, &samples, false);
        two_track_media(&path, &first, &second, 0);
        let (probe, inventory) = AudioProbe::for_offline_audit(&path).unwrap();
        let result = recover_audio(&probe, inventory.streams.len(), 4000, &Log::new(true)).unwrap();
        assert_eq!(result.info.audio_stream, 1);
        assert_eq!(result.info.audio_channel, None);
        assert_eq!(result.trials.len(), 4);
        for (trial, channel) in result.trials[..3].iter().zip([None, Some(0), Some(1)]) {
            assert_eq!(trial.audio_stream, 0);
            assert_eq!(trial.audio_channel, channel);
            assert_eq!(trial.independent_events, None);
            assert!(trial.error.is_some());
        }
        assert_eq!(result.trials[3].audio_stream, 1);
        assert_eq!(result.trials[3].audio_channel, None);
        assert_eq!(result.trials[3].independent_events, Some(2));
        assert!(result.trials[3].error.is_none());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| { warning.contains("could not assess stream 0 downmix") })
        );

        let (bad_probe, bad_inventory) = AudioProbe::for_offline_audit(&first).unwrap();
        let error = recover_audio(
            &bad_probe,
            bad_inventory.streams.len(),
            4000,
            &Log::new(true),
        )
        .err()
        .expect("a file with no completed source must still fail");
        assert!(error.contains("could not decode and measure any audio stream"));
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_channel_recovery_restores_two_canceled_aircraft() {
        let dir = tempfile::tempdir().unwrap();
        let samples = recording(true);
        let result = recover_fixture(&dir.path().join("two-aircraft.wav"), &samples);
        assert_eq!(result.info.audio_stream, 0);
        assert_eq!(result.info.audio_channel, Some(0));
        assert_eq!(result.pcm, samples);
        assert_eq!(result.trials.len(), 2, "stop at the first complete source");
        assert_eq!(result.trials[0].independent_events, Some(0));
        assert_eq!(result.trials[1].independent_events, Some(2));
        assert!(result.trials.iter().all(|trial| trial.error.is_none()));
        let events = search::independent_measured_events(&result.measurements.events, 0.0);
        assert_eq!(events.len(), 2);
        for (tone, heard) in [(83.0, 19.37), (450.0, 9.13)] {
            let event = events
                .iter()
                .find(|event| (event.f0 - tone).abs() < 0.3)
                .expect("the original physical aircraft must survive channel selection");
            assert!(
                (event.t_rec - heard).abs() < 0.3,
                "selection changed the recording clock: {event:?}"
            );
        }
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_channel_recovery_never_pools_duplicate_passes() {
        let dir = tempfile::tempdir().unwrap();
        let samples = recording(false);
        let result = recover_fixture(&dir.path().join("one-aircraft.wav"), &samples);
        assert_eq!(result.trials.len(), 3);
        assert_eq!(result.trials[0].independent_events, Some(0));
        assert_eq!(result.trials[1].independent_events, Some(1));
        assert_eq!(result.trials[2].independent_events, Some(1));
        assert_eq!(
            result.info.audio_channel,
            Some(0),
            "ties retain the first source"
        );
        assert_eq!(result.pcm, samples);
        assert_eq!(
            search::independent_measured_events(&result.measurements.events, 0.0).len(),
            1,
            "the same pass on two channels cannot manufacture a second aircraft"
        );
    }

    #[test]
    #[ignore = "requires ffmpeg 7.1+ and ffprobe"]
    fn automatic_channel_recovery_preserves_a_usable_downmix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in-phase-aircraft.wav");
        stereo_wav(&path, &recording(true), false);
        let (probe, _) = AudioProbe::for_offline_audit(&path)
            .expect("ffprobe is required by audio-source recovery");
        let selection = AudioSelection::default();
        let initial = audio::decode_probed(&probe, 0.0, None, 4000, selection)
            .expect("ffmpeg is required by audio-source recovery");
        let expected = initial.clone();
        let result = recover_channels(&probe, initial, selection, 4000, &Log::new(true)).unwrap();
        assert_eq!(result.info.audio_stream, 0);
        assert_eq!(result.info.audio_channel, None);
        assert_eq!(result.pcm, expected);
        assert_eq!(
            result.trials.len(),
            1,
            "a usable downmix needs no channel trial"
        );
        assert_eq!(result.trials[0].independent_events, Some(2));
        assert!(result.trials[0].error.is_none());
        assert!(result.warnings.is_empty());
        assert_eq!(
            search::independent_measured_events(&result.measurements.events, 0.0).len(),
            2
        );
    }
}
