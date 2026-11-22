//! The 3D rendering switches (ADR-0170 decision 1): which of the costly 3D effects the
//! engine draws, for every scene at once. An application setting, like the warm budget:
//! `studio.set_rendering` changes it and `studio.get` reports it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which of the costly 3D effects the engine draws (ADR-0170 decision 1, since 0.136).
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
}

fn on() -> bool {
    true
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
        }
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
    fn a_switch_an_older_server_leaves_out_reads_as_on() {
        let read: RenderFeatures = serde_json::from_str(r#"{"shadows":false}"#).expect("parses");
        assert_eq!(
            read,
            RenderFeatures {
                shadows: false,
                ..RenderFeatures::ALL
            }
        );
    }
}
