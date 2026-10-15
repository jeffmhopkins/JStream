//! Helpers for writing kind descriptors, and the kinds that are only described so far.

use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{KindCategory, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::Value;

/// A source kind descriptor with no parameters yet.
pub(crate) fn kind(id: &str, name: &str, video: bool, audio: bool) -> KindDescriptor {
    KindDescriptor {
        kind_id: id.into(),
        category: KindCategory::Source,
        name: name.into(),
        version: 1,
        video,
        audio,
        params: vec![],
    }
}

/// A parameter that restarts the source when it changes; callers mark live ones.
pub(crate) fn param(id: &str, label: &str, ty: ParamType, default: Value) -> ParamDescriptor {
    ParamDescriptor {
        id: id.into(),
        ty,
        default,
        label: label.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: false,
    }
}

/// A width or height in pixels.
pub(crate) fn size() -> ParamType {
    ParamType::Int {
        min: Some(1),
        max: Some(16384),
        step: Some(1),
        unit: Some("px".into()),
    }
}

/// An asset parameter accepting these file extensions.
pub(crate) fn asset(accept: &[&str]) -> ParamType {
    ParamType::Asset {
        accept: accept.iter().map(|&s| s.to_owned()).collect(),
    }
}

/// A kind that is described, so the collection can hold its sources and validate their
/// settings, but has no implementation yet: its sources fail to start with
/// [`SourceError::NotBuilt`]. Each is replaced by its roadmap item (webcam C4, media
/// file M2).
pub(crate) struct Described(pub(crate) KindDescriptor);

impl SourceKind for Described {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, _: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        Err(SourceError::NotBuilt {
            kind_id: self.0.kind_id.clone(),
        })
    }
}

/// The described-only built-in kinds, with the settings B4 gave them.
pub(crate) fn described() -> Vec<Described> {
    vec![
        Described(KindDescriptor {
            params: vec![param(
                "file",
                "Media file",
                asset(&["mp4", "mkv", "mov", "webm", "mp3", "wav", "flac", "ogg"]),
                Value::Null,
            )],
            ..kind("media", "Media file", true, true)
        }),
        Described(kind("webcam", "Webcam", true, false)),
    ]
}
