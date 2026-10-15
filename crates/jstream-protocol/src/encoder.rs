//! Encoder profiles: how an output's video and audio are encoded, as the collection
//! stores them (docs/ARCHITECTURE.md#data-model), and how the running outputs share
//! encoder sessions ([ADR-0027](../../../docs/adr/0027-program-master-and-output-chains.md)).
//!
//! A profile is collection state, changed by edits (`encoder.create`, `encoder.update`,
//! `encoder.delete`), and an output names the one it uses with `output.set_encoder`.
//! Running outputs that encode the same mix with the same profile share one encoder
//! session; `output.status` reports, per output, which outputs it shares with.

use crate::id::{EncoderProfileId, OutputId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How an output's video and audio are encoded.
///
/// An output with no profile uses the engine's default, [`EncoderProfile::DEFAULT`]:
/// H.264 at a constant 2500 kbit/s with a keyframe every 2 s on the software encoder,
/// and AAC at 160 kbit/s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct EncoderProfile {
    /// The video encoding.
    pub video: VideoEncoding,
    /// The audio encoding.
    pub audio: AudioEncoding,
}

impl EncoderProfile {
    /// What an output with no profile uses.
    pub const DEFAULT: EncoderProfile = EncoderProfile {
        video: VideoEncoding {
            codec: VideoCodec::H264,
            rate_control: RateControl::Cbr { kbps: 2500 },
            keyframe_interval_s: 2,
            hardware: HardwarePreference::Avoid,
        },
        audio: AudioEncoding {
            codec: AudioCodec::Aac,
            kbps: 160,
        },
    };
}

/// How the video is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct VideoEncoding {
    /// The codec.
    pub codec: VideoCodec,
    /// How the encoder spends bits. Rejected with `invalid_argument` outside
    /// 100..=100000 kbit/s, or, for `vbr`, with a peak below the average or above
    /// 200000 kbit/s.
    pub rate_control: RateControl,
    /// Seconds from one keyframe to the next, 1..=10, else `invalid_argument`. Most
    /// streaming services ask for 2.
    pub keyframe_interval_s: u32,
    /// Whether to use a hardware encoder. Omitted means `avoid`.
    #[serde(default)]
    pub hardware: HardwarePreference,
}

/// A video codec. Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VideoCodec {
    /// H.264 (AVC).
    H264,
    /// A codec this version of the protocol doesn't know, from a newer server. The
    /// engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// How a video encoder spends bits. Serialized with an internal `type` tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RateControl {
    /// Constant bit rate, as RTMP ingest expects.
    Cbr {
        /// The target, in kilobits per second.
        kbps: u32,
    },
    /// Variable bit rate with a peak, for a recording.
    Vbr {
        /// The average target, in kilobits per second.
        kbps: u32,
        /// The peak, in kilobits per second; at least `kbps`.
        max_kbps: u32,
    },
    /// A rate control this version of the protocol doesn't know, from a newer server.
    /// The engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// Whether a video encoder runs on the GPU's encoder block. Serialized as a snake_case
/// string. Hardware encoders limit how many sessions run at once, per GPU vendor and
/// driver; the engine doesn't guess that limit, and `output.status` reports the
/// hardware sessions in use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HardwarePreference {
    /// Hardware if there is one, else software.
    Prefer,
    /// Hardware only: `output.start` fails when there is none.
    Require,
    /// Software only.
    #[default]
    Avoid,
    /// A preference this version of the protocol doesn't know, from a newer server.
    /// The engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// How the audio is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct AudioEncoding {
    /// The codec.
    pub codec: AudioCodec,
    /// The bit rate, in kilobits per second: 32..=320, else `invalid_argument`.
    pub kbps: u32,
}

/// An audio codec. Serialized as a snake_case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AudioCodec {
    /// AAC-LC.
    Aac,
    /// A codec this version of the protocol doesn't know, from a newer server. The
    /// engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// One encoder profile, as `encoder.list` and `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EncoderProfileInfo {
    /// The profile's ID.
    pub profile_id: EncoderProfileId,
    /// Its display name.
    pub name: String,
    /// The encoding it describes.
    pub profile: EncoderProfile,
}

/// The encoder session a running output uses, as `output.status` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EncoderSessionInfo {
    /// The profile the session was opened with, or absent for the engine's default.
    /// The profile's settings at the time count: an output started after
    /// `encoder.update` gets a session of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<EncoderProfileId>,
    /// The other running outputs that share this session, ordered by ID. Empty when
    /// the output has the session to itself.
    #[serde(default)]
    pub shared_with: Vec<OutputId>,
    /// Whether the session's video encoder is a hardware one.
    pub hardware: bool,
}

#[cfg(test)]
mod tests {
    use super::{EncoderProfile, HardwarePreference, RateControl, VideoCodec};
    use serde_json::json;

    #[test]
    fn default_profile_wire_form() {
        assert_eq!(
            serde_json::to_value(EncoderProfile::DEFAULT).ok(),
            Some(json!({
                "video": {
                    "codec": "h264",
                    "rate_control": {"type": "cbr", "kbps": 2500},
                    "keyframe_interval_s": 2,
                    "hardware": "avoid"
                },
                "audio": {"codec": "aac", "kbps": 160}
            }))
        );
    }

    /// `hardware` may be left out, and values from a newer server parse as `unknown`.
    #[test]
    fn omitted_and_later_values() {
        let profile: EncoderProfile = serde_json::from_value(json!({
            "video": {
                "codec": "av1",
                "rate_control": {"type": "cqp", "qp": 20},
                "keyframe_interval_s": 2
            },
            "audio": {"codec": "aac", "kbps": 128}
        }))
        .expect("parses");
        assert_eq!(profile.video.codec, VideoCodec::Unknown);
        assert_eq!(profile.video.rate_control, RateControl::Unknown);
        assert_eq!(profile.video.hardware, HardwarePreference::Avoid);
    }
}
