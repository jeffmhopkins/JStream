//! Filter chains (ADR-0026, docs/adr/0026-source-filter-chains.md).
//!
//! A source has a video chain and an audio chain, each an ordered list of filters. A
//! filter is a kind from `kind.list` (a `video_filter` or `audio_filter` kind), an
//! on/off switch and settings checked against the kind's descriptor, like a source's
//! (ADR-0025). The `filter.*` edits address a filter by its owner and its ID.
//!
//! Sources own chains, and so do the master stage and each output (ADR-0080). Since
//! 0.61 the master's and every output's video chain take filters too (earlier minors
//! refused them). Since 0.43 a scene and an item also name a video chain (ADR-0065): a scene's
//! filters run on its whole composite, and an item's effects on its cropped, fitted box.

use crate::id::{FilterId, ItemId, OutputId, SceneId, SourceId};
use crate::settings::Settings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most filters one chain holds: the mixer's capacity per chain (ADR-0026 §1).
pub const MAX_CHAIN_FILTERS: usize = 8;

/// What owns a filter chain. Serialized with an internal `type` tag. Sources, the master
/// stage, outputs, scenes and items own chains so far; buses may be further variants.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FilterOwner {
    /// A source's chains, which apply wherever it is placed (ADR-0023).
    Source {
        /// The source.
        source_id: SourceId,
    },
    /// The master stage's chains, which apply to everything leaving JStream: every
    /// output, after the bus it takes (ADR-0080 §1). Not the `master` bus, whose
    /// fader feeds it. Its audio chain runs once per block on each bus that feeds an
    /// output, and monitoring hears the bus before it. Its video chain (since 0.61)
    /// runs once per frame on each mix an output draws, at the canvas size; the
    /// editing canvas, previews and `video.snapshot` show the mix before it. Its
    /// filters can't hold a reference.
    Master,
    /// An output's own chains, which run after the master stage for that output only
    /// (ADR-0080 §2). An output with filters on its audio chain gets its own audio
    /// encode from its next start; edits to a chain it started with apply live. Every
    /// output, the virtual camera included, has a video chain (since 0.61): one with
    /// a filter on it, even a disabled one, gets its own render pass and encoder
    /// session from its next start, which `output.status` reports as `dedicated`.
    Output {
        /// The output.
        output_id: OutputId,
    },
    /// A scene's filters, which run on the scene's whole composite wherever it renders
    /// (ADR-0065 §5, since 0.43). A video chain only: `chain: "audio"` is
    /// `invalid_argument` on `chain`. Never a keyframe scene, which shows its parent's.
    /// The engine runs this chain on the scene's composite.
    Scene {
        /// The scene.
        scene_id: SceneId,
    },
    /// An item's effects, which run on that one placement's cropped, fitted box, before
    /// its opacity and blend (ADR-0065 §5, since 0.43). A video chain only. Item IDs are
    /// unique across scenes, so no scene is named. The engine runs them over the item's
    /// box, in a layer grown by their kinds' `max_outset_px`, once per frame.
    Item {
        /// The item.
        item_id: ItemId,
    },
}

impl FilterOwner {
    /// The source that owns the chain, or `None` for another owner.
    pub fn source_id(&self) -> Option<&SourceId> {
        match self {
            FilterOwner::Source { source_id } => Some(source_id),
            FilterOwner::Master
            | FilterOwner::Output { .. }
            | FilterOwner::Scene { .. }
            | FilterOwner::Item { .. } => None,
        }
    }
}

/// Which of an owner's chains a filter is on. Every owner has at most these two, so the
/// set is closed.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FilterChain {
    /// The video chain: `video_filter` kinds. A source has one if its kind produces
    /// video.
    Video,
    /// The audio chain: `audio_filter` kinds, run before the source's fader
    /// (ADR-0028). A source has one if its kind produces audio and it is not a `scene`
    /// source.
    Audio,
}

impl FilterChain {
    /// The chain's wire name (`video`).
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterChain::Video => "video",
            FilterChain::Audio => "audio",
        }
    }
}

/// One filter, as `source.list`, `output.list` and `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterInfo {
    /// Its ID, unique within its owner's chains.
    pub filter_id: FilterId,
    /// Its kind, as `kind.list` reports it.
    pub kind: String,
    /// Whether it runs. A disabled filter is bypassed and keeps its settings.
    pub enabled: bool,
    /// Its settings: every parameter of its kind. Omitted for a kind without
    /// parameters.
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
}

/// An owner's two chains, each first filter first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterChains {
    /// The video chain. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video: Vec<FilterInfo>,
    /// The audio chain. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<FilterInfo>,
}

impl FilterChains {
    /// Whether both chains are empty, as `SourceInfo` and `OutputInfo` then omit them.
    pub fn is_empty(&self) -> bool {
        self.video.is_empty() && self.audio.is_empty()
    }

    /// The chain `chain`.
    pub fn chain(&self, chain: FilterChain) -> &[FilterInfo] {
        match chain {
            FilterChain::Video => &self.video,
            FilterChain::Audio => &self.audio,
        }
    }
}
