//! The encoder argument sets. The supervisor streams with them and the control process probes with them,
//! so a test encode proves the arguments the stream will use.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoder {
    #[serde(rename = "h264_nvenc")]
    Nvenc,
    #[serde(rename = "h264_amf")]
    Amf,
    #[serde(rename = "h264_qsv")]
    Qsv,
    #[serde(rename = "libx264")]
    Libx264,
}

impl Encoder {
    pub fn as_str(self) -> &'static str {
        match self {
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Amf => "h264_amf",
            Encoder::Qsv => "h264_qsv",
            Encoder::Libx264 => "libx264",
        }
    }

    pub fn parse(codec: &str) -> Option<Encoder> {
        [Encoder::Nvenc, Encoder::Amf, Encoder::Qsv, Encoder::Libx264].into_iter().find(|e| e.as_str() == codec)
    }
}

/// Spike 2's per-vendor video sets: CBR 4500k, fixed 60-frame GOP, no B-frames, High profile.
pub fn video_set(e: Encoder) -> &'static [&'static str] {
    match e {
        Encoder::Nvenc => &["-preset", "p4", "-rc", "cbr", "-bf", "0", "-profile:v", "high"],
        Encoder::Amf => &["-usage", "transcoding", "-quality", "balanced", "-rc", "cbr", "-bf", "0", "-profile:v", "high"],
        // QSV has no -rc: -b:v equal to -maxrate selects CBR.
        Encoder::Qsv => &["-preset", "medium", "-look_ahead", "0", "-bf", "0", "-profile:v", "high"],
        Encoder::Libx264 => &["-preset", "veryfast", "-sc_threshold", "0", "-keyint_min", "60"],
    }
}

/// What follows the video set in every encode, between the codec and the filters.
pub const PROBE_TAIL: &[&str] = &["-b:v", "4500k", "-maxrate", "4500k", "-bufsize", "9000k", "-g", "60", "-pix_fmt", "yuv420p"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_serialises_as_the_ffmpeg_codec_name() {
        // Arrange
        let all = [(Encoder::Nvenc, "h264_nvenc"), (Encoder::Amf, "h264_amf"), (Encoder::Qsv, "h264_qsv"), (Encoder::Libx264, "libx264")];
        for (e, name) in all {
            // Act
            let json = serde_json::to_string(&e).unwrap();
            // Assert
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<Encoder>(&json).unwrap(), e);
            assert_eq!(Encoder::parse(name), Some(e));
            assert_eq!(e.as_str(), name);
        }
    }

    #[test]
    fn parse_rejects_an_unknown_codec() {
        // Assert
        assert_eq!(Encoder::parse("h265_nope"), None);
    }

    #[test]
    fn video_sets_carry_each_vendors_exact_arguments() {
        // Assert
        assert_eq!(video_set(Encoder::Nvenc), ["-preset", "p4", "-rc", "cbr", "-bf", "0", "-profile:v", "high"]);
        assert_eq!(video_set(Encoder::Libx264), ["-preset", "veryfast", "-sc_threshold", "0", "-keyint_min", "60"]);
    }
}
