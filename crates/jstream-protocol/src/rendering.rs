//! The 3D rendering switches (ADR-0170 decision 1): which of the costly 3D effects the
//! engine draws, for every scene at once. An application setting, like the warm budget:
//! `studio.set_rendering` changes it and `studio.get` reports it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which of the costly 3D effects the engine draws (ADR-0170 decision 1, since 0.147).
///
/// A switch caps what every scene may use. Off, the effect is drawn nowhere, whatever a
/// scene, light or filter asks for; on, each scene gets what it asks for. No switch
/// changes the collection, so turning one back on restores every scene as it was. Every
/// switch is on until `studio.set_rendering` turns it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct RenderFeatures {
    /// Shadow maps (ADR-0151 decision 3). Off, no light casts a shadow, whatever its
    /// `shadows` setting.
    #[serde(default = "on")]
    pub shadows: bool,
    /// The `ambient_occlusion` filter (ADR-0151 decision 5). Off, it passes its input
    /// through.
    #[serde(default = "on")]
    pub ambient_occlusion: bool,
    /// The `volumetric_fog` filter (ADR-0151 decision 6). Off, it passes its input
    /// through.
    #[serde(default = "on")]
    pub volumetric_fog: bool,
    /// The `floor_reflection` filter (ADR-0151 decision 4), which draws its scene a
    /// second time. Off, it passes its input through and the scene isn't redrawn.
    #[serde(default = "on")]
    pub floor_reflections: bool,
    /// Reflection probes (ADR-0170 decision 3, since 0.159), which draw their scene six
    /// times a capture. Off, no probe captures and every surface reflects its
    /// environment as without one.
    #[serde(default = "on")]
    pub reflection_probes: bool,
    /// The largest face a probe captures at, in pixels a side: one of
    /// [`PROBE_RESOLUTIONS`] (since 0.159). A probe set larger captures at this.
    #[serde(default = "max_probe")]
    pub max_probe_resolution: u32,
}

/// The sizes a reflection probe's faces may be, in pixels a side (ADR-0170 decision 3).
pub const PROBE_RESOLUTIONS: [u32; 3] = [128, 256, 512];

fn on() -> bool {
    true
}

fn max_probe() -> u32 {
    PROBE_RESOLUTIONS[PROBE_RESOLUTIONS.len() - 1]
}

impl Default for RenderFeatures {
    fn default() -> Self {
        Self::ALL
    }
}

impl RenderFeatures {
    /// Every switch on: what the engine draws until `studio.set_rendering` changes it.
    pub const ALL: Self = Self {
        shadows: true,
        ambient_occlusion: true,
        volumetric_fog: true,
        floor_reflections: true,
        reflection_probes: true,
        max_probe_resolution: 512,
    };

    /// Whether a scene filter of `kind_id` is drawn: `false` only for a filter kind a
    /// switch that is off names.
    #[must_use]
    pub fn draws_filter(&self, kind_id: &str) -> bool {
        match kind_id {
            "ambient_occlusion" => self.ambient_occlusion,
            "volumetric_fog" => self.volumetric_fog,
            "floor_reflection" => self.floor_reflections,
            _ => true,
        }
    }

    /// These switches with each one `change` names set to its value, and the rest kept.
    #[must_use]
    pub fn with(self, change: &RenderFeaturesChange) -> Self {
        Self {
            shadows: change.shadows.unwrap_or(self.shadows),
            ambient_occlusion: change.ambient_occlusion.unwrap_or(self.ambient_occlusion),
            volumetric_fog: change.volumetric_fog.unwrap_or(self.volumetric_fog),
            floor_reflections: change.floor_reflections.unwrap_or(self.floor_reflections),
            reflection_probes: change.reflection_probes.unwrap_or(self.reflection_probes),
            max_probe_resolution: change
                .max_probe_resolution
                .unwrap_or(self.max_probe_resolution),
        }
    }

    /// The face size a probe set to `resolution` captures at: the largest of
    /// [`PROBE_RESOLUTIONS`] no larger than both it and [`Self::max_probe_resolution`].
    #[must_use]
    pub fn probe_face(&self, resolution: u32) -> u32 {
        let cap = resolution.min(self.max_probe_resolution);
        PROBE_RESOLUTIONS
            .iter()
            .copied()
            .rev()
            .find(|&size| size <= cap)
            .unwrap_or(PROBE_RESOLUTIONS[0])
    }
}

/// What `studio.set_rendering` changes: each switch it names, and none it leaves out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenderFeaturesChange {
    /// Whether lights set to cast shadows draw them, or left out to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<bool>,
    /// Whether the `ambient_occlusion` filter draws, or left out to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient_occlusion: Option<bool>,
    /// Whether the `volumetric_fog` filter draws, or left out to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volumetric_fog: Option<bool>,
    /// Whether the `floor_reflection` filter draws, or left out to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor_reflections: Option<bool>,
    /// Whether reflection probes capture, or left out to keep it (since 0.159).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflection_probes: Option<bool>,
    /// The largest face a probe captures at, one of [`PROBE_RESOLUTIONS`], or left out
    /// to keep it (since 0.159). `studio.set_rendering` refuses any other size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_probe_resolution: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::{RenderFeatures, RenderFeaturesChange};

    #[test]
    fn a_change_sets_what_it_names_and_keeps_the_rest() {
        let off = RenderFeatures::ALL.with(&RenderFeaturesChange {
            shadows: Some(false),
            floor_reflections: Some(false),
            ..RenderFeaturesChange::default()
        });
        assert!(!off.shadows && !off.floor_reflections);
        assert!(off.ambient_occlusion && off.volumetric_fog);
        assert_eq!(off.with(&RenderFeaturesChange::default()), off);
        assert!(!off.draws_filter("floor_reflection"));
        assert!(off.draws_filter("ambient_occlusion"));
        assert!(off.draws_filter("depth_fog"), "a kind no switch names");
    }

    #[test]
    fn a_probe_captures_at_its_size_within_the_cap() {
        let capped = RenderFeatures {
            max_probe_resolution: 256,
            ..RenderFeatures::ALL
        };
        assert_eq!(RenderFeatures::ALL.probe_face(512), 512);
        assert_eq!(capped.probe_face(512), 256);
        assert_eq!(capped.probe_face(128), 128);
        assert_eq!(capped.probe_face(200), 128, "between sizes, the one below");
        assert_eq!(capped.probe_face(0), 128, "never below the smallest");
    }

    #[test]
    fn a_switch_an_older_server_leaves_out_reads_as_on() {
        let read: RenderFeatures = serde_json::from_str(r#"{"shadows":false}"#).expect("parses");
        assert_eq!(
            read,
            RenderFeatures {
                shadows: false,
                ..RenderFeatures::ALL
            }
        );
        assert!(read.reflection_probes && read.max_probe_resolution == 512);
    }
}
