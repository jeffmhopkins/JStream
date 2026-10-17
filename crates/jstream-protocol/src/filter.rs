//! Filter chains (ADR-0026, docs/adr/0026-source-filter-chains.md).
//!
//! A source has a video chain and an audio chain, each an ordered list of filters. A
//! filter is a kind from `kind.list` (a `video_filter` or `audio_filter` kind), an
//! on/off switch and settings checked against the kind's descriptor, like a source's
//! (ADR-0025). The `filter.*` edits address a filter by its owner and its ID.

use crate::id::{FilterId, SourceId};
use crate::settings::Settings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most filters one chain holds: the mixer's capacity per chain (ADR-0026 §1).
pub const MAX_CHAIN_FILTERS: usize = 8;

/// What owns a filter chain. Serialized with an internal `type` tag. Only sources own
/// chains so far; buses, scenes and outputs will be further variants.
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
}

impl FilterOwner {
    /// The source that owns the chain.
    pub fn source_id(&self) -> &SourceId {
        match self {
            FilterOwner::Source { source_id } => source_id,
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

/// One filter, as `source.list` and `state.get` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilterInfo {
    /// Its ID, unique within its source.
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

/// A source's two chains, each first filter first.
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
    /// Whether both chains are empty, as `SourceInfo` then omits them.
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
