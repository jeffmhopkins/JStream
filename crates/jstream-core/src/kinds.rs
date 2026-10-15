//! The built-in source kinds the engine describes (`kind.list`, `kind.describe`) and
//! validates settings against (ADR-0025, docs/adr/0025-typed-settings-kind-descriptors.md).
//!
//! These are stubs: they name the built-in kinds and give the ones that obviously need
//! settings (a colour, a file, some text) a first set of parameters. Nothing renders
//! from them yet; frames still come from `Engine::set_source_frame`. The `jstream-sources`
//! crate replaces this list with the real kinds and their own descriptor snapshot.

use jstream_protocol::{KindCategory, KindDescriptor, ParamDescriptor, ParamType};
use serde_json::{Value, json};

/// Every built-in source kind, ordered by ID.
pub(crate) fn builtin() -> Vec<KindDescriptor> {
    vec![
        KindDescriptor {
            params: vec![
                ParamDescriptor {
                    live: true,
                    ..param(
                        "color",
                        "Colour",
                        ParamType::Color { alpha: true },
                        json!("#000000ff"),
                    )
                },
                param("width", "Width", size(), json!(1920)),
                param("height", "Height", size(), json!(1080)),
            ],
            ..kind("color", "Colour", true, false)
        },
        kind("display", "Display capture", true, false),
        KindDescriptor {
            params: vec![param(
                "file",
                "Image file",
                ParamType::Asset {
                    accept: ["png", "jpg", "jpeg", "webp", "gif"]
                        .map(String::from)
                        .into(),
                },
                Value::Null,
            )],
            ..kind("image", "Image", true, false)
        },
        KindDescriptor {
            params: vec![param(
                "file",
                "Media file",
                ParamType::Asset {
                    accept: ["mp4", "mkv", "mov", "webm", "mp3", "wav", "flac", "ogg"]
                        .map(String::from)
                        .into(),
                },
                Value::Null,
            )],
            ..kind("media", "Media file", true, true)
        },
        KindDescriptor {
            params: vec![
                ParamDescriptor {
                    live: true,
                    ..param(
                        "text",
                        "Text",
                        ParamType::String {
                            multiline: true,
                            max_len: 4096,
                        },
                        json!(""),
                    )
                },
                ParamDescriptor {
                    live: true,
                    ..param(
                        "color",
                        "Colour",
                        ParamType::Color { alpha: true },
                        json!("#ffffffff"),
                    )
                },
            ],
            ..kind("text", "Text", true, false)
        },
        kind("webcam", "Webcam", true, false),
        kind("window", "Window capture", true, false),
    ]
}

fn kind(id: &str, name: &str, video: bool, audio: bool) -> KindDescriptor {
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
fn param(id: &str, label: &str, ty: ParamType, default: Value) -> ParamDescriptor {
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

fn size() -> ParamType {
    ParamType::Int {
        min: Some(1),
        max: Some(16384),
        step: Some(1),
        unit: Some("px".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::builtin;
    use jstream_protocol::parity;
    use std::collections::BTreeSet;

    #[test]
    fn ids_are_sorted_and_unique_and_defaults_are_valid() {
        let kinds = builtin();
        let ids: Vec<&str> = kinds.iter().map(|k| k.kind_id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted);
        for kind in &kinds {
            let params: BTreeSet<&str> = kind.params.iter().map(|p| p.id.as_str()).collect();
            assert_eq!(params.len(), kind.params.len(), "{}", kind.kind_id);
            for p in &kind.params {
                assert_eq!(p.check(&p.default), Ok(()), "{}.{}", kind.kind_id, p.id);
            }
        }
    }

    /// The shared parity collection needs its kinds described exactly as the fixture
    /// does, so every implementation reports the same settings.
    #[test]
    fn parity_kinds_are_builtin() {
        let kinds = builtin();
        for kind in parity::kinds() {
            assert!(kinds.contains(&kind), "{}", kind.kind_id);
        }
    }
}
