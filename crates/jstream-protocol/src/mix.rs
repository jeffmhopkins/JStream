//! Video mixes: what an output's video shows, as the collection stores it
//! (docs/ARCHITECTURE.md#data-model).
//!
//! A mix renders one input (the program, the preview, or a scene) with the canvas fitted
//! inside its resolution ([ADR-0022](../../../docs/adr/0022-item-transform-and-canvas.md)),
//! on every `fps_divisor`th tick of the engine's base frame rate. It is collection
//! state, changed by edits (`video_mix.create`, `video_mix.update`, `video_mix.delete`),
//! and an output names the one it shows with `output.set_video_mix`. An output that names
//! none shows the program at the engine's capture size and base rate.

use crate::id::{SceneId, VideoMixId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A mix: which picture, at what size and rate.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct VideoMix {
    /// What it shows.
    pub input: MixInput,
    /// Its frame size in pixels. The canvas is fitted inside it, centred, with black
    /// bars where the aspect ratios differ, never stretched (ADR-0022).
    pub resolution: MixResolution,
    /// Renders every `fps_divisor`th tick of the engine's base frame rate: 1 is the base
    /// rate, 2 half of it. Missing parses as 1.
    #[serde(default = "VideoMix::base_rate")]
    pub fps_divisor: u32,
}

/// The size of a mix's frames, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MixResolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// What a mix shows. Serialized with an internal `type` tag.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MixInput {
    /// The scene on program (ADR-0030): it follows `scene.switch`.
    Program,
    /// The scene on preview. Until studio mode exists there is no preview scene, and a
    /// `preview` mix shows the program.
    Preview,
    /// One scene, whatever is on program. `scene.delete` is refused while a mix shows
    /// the scene.
    Scene {
        /// The scene shown.
        scene_id: SceneId,
    },
    /// An input this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// One mix, as `video_mix.list` and `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VideoMixInfo {
    /// The mix's ID.
    pub mix_id: VideoMixId,
    /// Its display name.
    pub name: String,
    /// What it shows, and at what size and rate.
    pub mix: VideoMix,
}

/// The sides a mix's resolution may have, in pixels. Even, because H.264's 4:2:0
/// chroma halves both sides.
const SIDE: std::ops::RangeInclusive<u32> = 16..=8192;
/// The divisors of the base frame rate a mix may ask for.
const DIVISOR: std::ops::RangeInclusive<u32> = 1..=60;

impl VideoMix {
    fn base_rate() -> u32 {
        1
    }

    /// Why an engine can't render this mix, as the field (relative to the mix,
    /// `resolution.width`) and the reason, or `None` when it can. Every engine and the
    /// mock check a mix with this, so they reject the same ones. Whether a `scene`
    /// input's scene exists is the engine's to check.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        if self.input == MixInput::Unknown {
            return Some(("input.type", "is not a mix input this engine knows".into()));
        }
        for (field, side) in [
            ("resolution.width", self.resolution.width),
            ("resolution.height", self.resolution.height),
        ] {
            if !SIDE.contains(&side) || side % 2 != 0 {
                return Some((
                    field,
                    format!(
                        "must be an even number from {} to {}",
                        SIDE.start(),
                        SIDE.end()
                    ),
                ));
            }
        }
        if !DIVISOR.contains(&self.fps_divisor) {
            return Some((
                "fps_divisor",
                format!("must be from {} to {}", DIVISOR.start(), DIVISOR.end()),
            ));
        }
        None
    }

    /// The scene a `scene` input shows.
    pub fn scene(&self) -> Option<&SceneId> {
        match &self.input {
            MixInput::Scene { scene_id } => Some(scene_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MixInput, MixResolution, VideoMix};
    use crate::id::SceneId;
    use serde_json::json;

    fn mix(width: u32, height: u32, fps_divisor: u32) -> VideoMix {
        VideoMix {
            input: MixInput::Program,
            resolution: MixResolution { width, height },
            fps_divisor,
        }
    }

    #[test]
    fn wire_form() {
        let mix = VideoMix {
            input: MixInput::Scene {
                scene_id: SceneId::new("cam"),
            },
            resolution: MixResolution {
                width: 1280,
                height: 720,
            },
            fps_divisor: 2,
        };
        let wire = json!({
            "input": {"type": "scene", "scene_id": "cam"},
            "resolution": {"width": 1280, "height": 720},
            "fps_divisor": 2
        });
        assert_eq!(serde_json::to_value(&mix).ok(), Some(wire.clone()));
        assert_eq!(serde_json::from_value::<VideoMix>(wire).ok(), Some(mix));
    }

    #[test]
    fn a_missing_divisor_is_the_base_rate_and_later_inputs_are_unknown() {
        let parsed = serde_json::from_value::<VideoMix>(json!({
            "input": {"type": "multiview", "cells": 4},
            "resolution": {"width": 640, "height": 360}
        }))
        .expect("parses");
        assert_eq!(parsed.fps_divisor, 1);
        assert_eq!(parsed.input, MixInput::Unknown);
        assert_eq!(parsed.problem().map(|(field, _)| field), Some("input.type"));
    }

    #[test]
    fn problems_name_the_field() {
        assert_eq!(mix(1920, 1080, 1).problem(), None);
        assert_eq!(mix(16, 8192, 60).problem(), None);
        let field = |m: VideoMix| m.problem().map(|(field, _)| field);
        assert_eq!(field(mix(1921, 1080, 1)), Some("resolution.width"));
        assert_eq!(field(mix(14, 1080, 1)), Some("resolution.width"));
        assert_eq!(field(mix(1920, 8194, 1)), Some("resolution.height"));
        assert_eq!(field(mix(1920, 1080, 0)), Some("fps_divisor"));
        assert_eq!(field(mix(1920, 1080, 61)), Some("fps_divisor"));
    }
}
