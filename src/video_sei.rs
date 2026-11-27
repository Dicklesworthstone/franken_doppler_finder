//! Strip supplemental enhancement information from the selected coded video stream.

use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

#[derive(Clone, Copy)]
pub(crate) enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub(crate) fn probe(path: &Path) -> Result<Self, String> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-select_streams",
                "V:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "default=nw=1:nk=1",
            ])
            .arg(path)
            .output()
            .map_err(|e| format!("cannot probe the video stream: {e}"))?;
        if !output.status.success() || !output.stderr.is_empty() {
            return Err(format!(
                "video stream probe failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "h264" => Ok(Self::H264),
            "hevc" => Ok(Self::Hevc),
            _ => Err("--strip-video-sei requires a primary H.264 or HEVC video stream".into()),
        }
    }

    pub(crate) fn strip_filter(self) -> &'static str {
        match self {
            Self::H264 => "filter_units=remove_types=6",
            Self::Hevc => "filter_units=remove_types=39|40",
        }
    }

    pub(crate) fn verify(self, path: &Path) -> Result<(), String> {
        // Inspect Annex B packets *and* the decoder configuration. filter_units
        // may preserve SEI arrays in an MP4's hvcC even after stripping packets.
        // dump_extra also exposes these arrays when the clip has no key packet.
        let (convert, mux) = match self {
            Self::H264 => ("h264_mp4toannexb,dump_extra=freq=all", "h264"),
            Self::Hevc => ("hevc_mp4toannexb,dump_extra=freq=all", "hevc"),
        };
        let mut child = Command::new("ffmpeg")
            .args([
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
            .arg(path)
            .args([
                "-map",
                "0:V:0",
                "-c:v",
                "copy",
                "-bsf:v",
                convert,
                "-f",
                mux,
                "-",
                // Require a decodable coded picture in addition to inspecting the NALs.
                "-map",
                "0:V:0",
                "-frames:v",
                "1",
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot verify the staged video: {e}"))?;
        let stream = child.stdout.take().expect("piped video");
        std::thread::scope(|scope| {
            let scan = scope.spawn(move || self.inspect_nals(stream));
            let result = child
                .wait_with_output()
                .map_err(|e| format!("video verification: {e}"))?;
            scan.join()
                .map_err(|_| "video verification reader failed")??;
            if !result.status.success() || !result.stderr.is_empty() {
                return Err(format!(
                    "cannot verify the staged video ({}): {}",
                    result.status,
                    String::from_utf8_lossy(&result.stderr).trim()
                ));
            }
            Ok(())
        })
    }

    fn inspect_nals(self, mut stream: impl Read) -> Result<(), String> {
        let mut zeros = 0_u8;
        let mut header = false;
        let mut picture = false;
        let mut buffer = [0; 65536];
        loop {
            let n = stream
                .read(&mut buffer)
                .map_err(|e| format!("reading staged video: {e}"))?;
            if n == 0 {
                break;
            }
            for &byte in &buffer[..n] {
                if header {
                    let (sei, coded) = match self {
                        Self::H264 => (byte & 31 == 6, matches!(byte & 31, 1..=5)),
                        Self::Hevc => (
                            matches!((byte >> 1) & 63, 39 | 40),
                            matches!((byte >> 1) & 63, 0..=9 | 16..=21),
                        ),
                    };
                    if sei {
                        return Err(
                            "staged video still carries SEI in its packets or codec configuration"
                                .into(),
                        );
                    }
                    picture |= coded;
                    header = false;
                } else if byte == 1 && zeros >= 2 {
                    header = true;
                }
                zeros = if byte == 0 {
                    zeros.saturating_add(1)
                } else {
                    0
                };
            }
        }
        if !picture || header {
            return Err(
                "staged video has no verifiable coded pictures or ends in a partial NAL header"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_suffix_sei_across_read_boundaries_and_requires_coded_pictures() {
        let mut stream = vec![0_u8, 0, 1, 2, 1, 0x80]; // HEVC TRAIL_R picture
        stream.resize(65534, 0xaa);
        stream.extend([0, 0, 1, 80, 1, 0x80]); // HEVC suffix SEI
        assert!(
            Codec::Hevc
                .inspect_nals(stream.as_slice())
                .unwrap_err()
                .contains("SEI")
        );
        assert!(
            Codec::H264
                .inspect_nals(&[0, 0, 1, 0x67, 0x42, 0][..])
                .is_err()
        );
        assert!(Codec::H264.inspect_nals(&[][..]).is_err());
    }
}
