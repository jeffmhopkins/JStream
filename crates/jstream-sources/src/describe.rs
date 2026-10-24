//! Helpers for writing kind descriptors.

use jstream_protocol::{KindCategory, KindDescriptor, ParamDescriptor, ParamType};
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
