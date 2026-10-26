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
/// H.264 at a constant 2500 kbit/s with a keyframe every 2 s on a hardware encoder when
/// there is one (else the software one), and AAC at 160 kbit/s.
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
            hardware: HardwarePreference::Prefer,
        },
        audio: AudioEncoding {
            codec: AudioCodec::Aac,
            kbps: 160,
        },
    };
}

/// The video bit rates a profile may ask for, in kbit/s.
const VIDEO_KBPS: std::ops::RangeInclusive<u32> = 100..=100_000;
/// The highest VBR peak, in kbit/s.
const MAX_PEAK_KBPS: u32 = 200_000;
/// Seconds between keyframes.
const KEYFRAME_S: std::ops::RangeInclusive<u32> = 1..=10;
/// The AAC bit rates a profile may ask for, in kbit/s.
const AUDIO_KBPS: std::ops::RangeInclusive<u32> = 32..=320;

impl EncoderProfile {
    /// Why an engine can't open this encoding, as the field (relative to the profile,
    /// `video.rate_control.kbps`) and the reason, or `None` when it can. Every engine
    /// and the mock check a profile with this, so they reject the same ones. Whether
    /// this machine has an encoder for the codec is not part of it: the profile is
    /// collection state, valid on any machine, and `output.start` fails when the
    /// machine has none.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        let video = &self.video;
        if video.codec == VideoCodec::Unknown {
            return Some((
                "video.codec",
                "is not a video codec this engine knows".into(),
            ));
        }
        let kbps = match video.rate_control {
            RateControl::Cbr { kbps } => kbps,
            RateControl::Vbr { kbps, max_kbps } => {
                if max_kbps < kbps || max_kbps > MAX_PEAK_KBPS {
                    return Some((
                        "video.rate_control.max_kbps",
                        format!("must be at least `kbps` and at most {MAX_PEAK_KBPS}"),
                    ));
                }
                kbps
            }
            RateControl::Unknown => {
                return Some((
                    "video.rate_control.type",
                    "is not a rate control this engine knows".into(),
                ));
            }
        };
        if !VIDEO_KBPS.contains(&kbps) {
            return Some(("video.rate_control.kbps", range(&VIDEO_KBPS)));
        }
        if !KEYFRAME_S.contains(&video.keyframe_interval_s) {
            return Some(("video.keyframe_interval_s", range(&KEYFRAME_S)));
        }
        if video.hardware == HardwarePreference::Unknown {
            return Some((
                "video.hardware",
                "is not a hardware preference this engine knows".into(),
            ));
        }
        if self.audio.codec != AudioCodec::Aac {
            return Some((
                "audio.codec",
                "is not an audio codec this engine knows".into(),
            ));
        }
        if !AUDIO_KBPS.contains(&self.audio.kbps) {
            return Some(("audio.kbps", range(&AUDIO_KBPS)));
        }
        None
    }
}

fn range(range: &std::ops::RangeInclusive<u32>) -> String {
    format!("must be in {}..={}", range.start(), range.end())
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

/// A video codec. Serialized as a snake_case string. Streams carry HEVC and AV1 in
/// enhanced RTMP; a recording takes H.264 only so far, and its start fails with another.
/// A machine with no encoder for the codec, or none that fits the profile's
/// `hardware`, fails `output.start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VideoCodec {
    /// H.264 (AVC).
    H264,
    /// HEVC (H.265). Since 0.41.
    Hevc,
    /// AV1. Since 0.41.
    Av1,
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
    /// Hardware if there is one, else software. What [`EncoderProfile::DEFAULT`] uses
    /// (ADR-0061: hardware first).
    Prefer,
    /// Hardware only: `output.start` fails when there is none.
    Require,
    /// Software only. What an omitted `hardware` field means.
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
    /// Whether the output has the session to itself by rule: an output with its own
    /// video chain never shares (ADR-0027 §2), so its picture costs one more GPU pass
    /// and one more encoder session. Omitted when false, which is every output until
    /// video chains exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dedicated: bool,
}

#[cfg(test)]
mod tests {
    use super::{EncoderProfile, HardwarePreference, RateControl, VideoCodec};
    use serde_json::json;

    #[test]
    fn the_default_profile_is_valid() {
        assert_eq!(EncoderProfile::DEFAULT.problem(), None);
    }

    #[test]
    fn default_profile_wire_form() {
        assert_eq!(
            serde_json::to_value(EncoderProfile::DEFAULT).ok(),
            Some(json!({
                "video": {
                    "codec": "h264",
                    "rate_control": {"type": "cbr", "kbps": 2500},
                    "keyframe_interval_s": 2,
                    "hardware": "prefer"
                },
                "audio": {"codec": "aac", "kbps": 160}
            }))
        );
    }

    /// HEVC and AV1 are accepted (0.41), with their wire names; an unknown codec is not.
    #[test]
    fn hevc_and_av1_are_accepted() {
        for (codec, name) in [
            (VideoCodec::H264, "h264"),
            (VideoCodec::Hevc, "hevc"),
            (VideoCodec::Av1, "av1"),
        ] {
            let mut profile = EncoderProfile::DEFAULT;
            profile.video.codec = codec;
            assert_eq!(profile.problem(), None, "{name}");
            assert_eq!(serde_json::to_value(codec).ok(), Some(json!(name)));
            assert_eq!(
                serde_json::from_value::<VideoCodec>(json!(name)).ok(),
                Some(codec)
            );
        }
        let mut profile = EncoderProfile::DEFAULT;
        profile.video.codec = VideoCodec::Unknown;
        assert_eq!(
            profile.problem().map(|(field, _)| field),
            Some("video.codec")
        );
    }

    /// `hardware` may be left out, and values from a newer server parse as `unknown`.
    #[test]
    fn omitted_and_later_values() {
        let profile: EncoderProfile = serde_json::from_value(json!({
            "video": {
                "codec": "vvc",
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
