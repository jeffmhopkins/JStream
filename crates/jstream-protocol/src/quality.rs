//! `quality.probe` (0.110, ADR-0132 §3 as ADR-0145 refines it): how close the pictures a
//! stream's decoder would show are to the frames its encoder was given, as PSNR and
//! SSIM.

use crate::error::ProtocolError;
use crate::finite::Finite32;
use crate::id::OutputId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The frames `quality.probe` compares when it names no `frames`.
pub const DEFAULT_PROBE_FRAMES: u32 = 3;
/// The most frames one `quality.probe` compares.
pub const MAX_PROBE_FRAMES: u32 = 10;
/// The PSNR reported for two identical pictures, in decibels, whose true PSNR is
/// infinite: no lossy stream comes near it.
pub const MAX_PSNR_DB: f32 = 100.0;

/// The frames to compare: `frames`, or [`DEFAULT_PROBE_FRAMES`] when omitted.
///
/// # Errors
/// `invalid_argument` on `frames` outside 1 to [`MAX_PROBE_FRAMES`].
pub fn probe_frames(frames: Option<u32>) -> Result<u32, ProtocolError> {
    let frames = frames.unwrap_or(DEFAULT_PROBE_FRAMES);
    if (1..=MAX_PROBE_FRAMES).contains(&frames) {
        Ok(frames)
    } else {
        Err(ProtocolError::invalid_argument(
            "frames",
            format!("a probe compares 1 to {MAX_PROBE_FRAMES} frames"),
        ))
    }
}

/// The encoded stream a `quality.probe` measures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProbeStream {
    /// A running output's video: the encoder session it sends from, which it may share
    /// with other outputs of the same mix and profile.
    Output {
        /// The output.
        output_id: OutputId,
    },
}

/// One frame a probe compared with its decoded picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FrameQuality {
    /// The frame's presentation time on the stream, in microseconds.
    pub pts_us: i64,
    /// PSNR of the luma plane, in dB, at most [`MAX_PSNR_DB`].
    pub psnr_y: Finite32,
    /// PSNR over every luma and chroma sample, in dB, at most [`MAX_PSNR_DB`].
    pub psnr: Finite32,
    /// SSIM of the luma plane, from 0 to 1 (1 is identical), over 8 × 8 windows.
    pub ssim_y: Finite32,
}

/// The result of `quality.probe`: each frame's PSNR and SSIM, and their means.
///
/// Both pictures are compared as NV12 in BT.709 limited range at the encoded size: the
/// frame the encoder was given, converted the way the engine's CPU encoders convert it,
/// and the picture the platform's decoder made of its packets. A missing field parses
/// as 0 or empty (R-API-001).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct QualityProbeInfo {
    /// The encoder that made the stream, as `encoder.list` names it.
    pub encoder: String,
    /// Whether it is a hardware encoder.
    pub hardware: bool,
    /// The decoder the engine decoded it with.
    pub decoder: String,
    /// The encoded width, in pixels.
    pub width: u32,
    /// The encoded height, in pixels.
    pub height: u32,
    /// The frames compared, in presentation order.
    pub frames: Vec<FrameQuality>,
    /// The mean of the frames' `psnr_y`.
    pub psnr_y: Finite32,
    /// The mean of the frames' `psnr`.
    pub psnr: Finite32,
    /// The mean of the frames' `ssim_y`.
    pub ssim_y: Finite32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_default_to_three_and_stay_within_one_to_ten() {
        assert_eq!(probe_frames(None).unwrap(), DEFAULT_PROBE_FRAMES);
        assert_eq!(probe_frames(Some(1)).unwrap(), 1);
        assert_eq!(
            probe_frames(Some(MAX_PROBE_FRAMES)).unwrap(),
            MAX_PROBE_FRAMES
        );
        for bad in [0, MAX_PROBE_FRAMES + 1] {
            let err = probe_frames(Some(bad)).unwrap_err();
            assert!(
                matches!(&err, ProtocolError::InvalidArgument { field, .. } if field == "frames"),
                "{err:?}"
            );
        }
    }

    #[test]
    fn a_stream_is_tagged_by_its_type() {
        let stream = ProbeStream::Output {
            output_id: OutputId::new("out_1"),
        };
        let json = serde_json::to_value(&stream).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "output", "output_id": "out_1"})
        );
        assert_eq!(serde_json::from_value::<ProbeStream>(json).unwrap(), stream);
    }
}
