//! Choose one observed audio channel before any flight or location search.
//!
//! The downmix keeps priority. Only a downmix with fewer than two independent measured
//! aircraft tries individual channels, in their original order. No observations are pooled.

use crate::pipeline::{AudioMeasurements, Log, measure_recording};
use fdop_core::audio::{self, AudioProbe, AudioSelection, MediaInfo};
use fdop_core::search;
use serde::Serialize;
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
        let candidate = (|| {
            let pcm = audio::decode_probed(probe, 0.0, None, sr, source)?;
            let info = probe.selected_info(source)?;
            let measurements = measure_recording(&pcm, sr, log, None)?;
            Ok::<_, String>((pcm, info, measurements))
        })();
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
            let other = if antiphase { -sample } else { sample };
            out.write_all(&other.to_le_bytes()).unwrap();
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
