//! What every filter kind shares: descriptor builders (ADR-0025), a reader that turns
//! complete settings into shader values, and a uniform-buffer writer that follows WGSL's
//! uniform layout.

use jstream_protocol::{
    EnumOption, KindCategory, KindDescriptor, ParamDescriptor, ParamType, Settings,
};
use serde_json::Value;

use crate::color::srgb_channel_to_linear;

/// A `video_filter` descriptor, version 1.
pub(crate) fn filter(id: &str, name: &str, params: Vec<ParamDescriptor>) -> KindDescriptor {
    KindDescriptor {
        kind_id: id.into(),
        category: KindCategory::VideoFilter,
        name: name.into(),
        version: 1,
        video: true,
        audio: false,
        params,
    }
}

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
        live: true,
    }
}

/// A live number from `min` to `max`, in `unit` (empty for none).
pub(crate) fn float(
    id: &str,
    label: &str,
    min: f64,
    max: f64,
    default: f64,
    unit: &str,
) -> ParamDescriptor {
    param(
        id,
        label,
        ParamType::Float {
            min: Some(min),
            max: Some(max),
            step: None,
            unit: (!unit.is_empty()).then(|| unit.into()),
        },
        Value::from(default),
    )
}

/// A live whole number from `min` to `max`, in `unit` (empty for none).
pub(crate) fn int(
    id: &str,
    label: &str,
    min: i64,
    max: i64,
    default: i64,
    unit: &str,
) -> ParamDescriptor {
    param(
        id,
        label,
        ParamType::Int {
            min: Some(min),
            max: Some(max),
            step: None,
            unit: (!unit.is_empty()).then(|| unit.into()),
        },
        Value::from(default),
    )
}

/// A live switch.
pub(crate) fn boolean(id: &str, label: &str, default: bool) -> ParamDescriptor {
    param(id, label, ParamType::Bool, Value::from(default))
}

/// A live choice among `options`, each a value and a label.
pub(crate) fn choice(
    id: &str,
    label: &str,
    options: &[(&str, &str)],
    default: &str,
) -> ParamDescriptor {
    param(
        id,
        label,
        ParamType::Enum {
            options: options
                .iter()
                .map(|(value, label)| EnumOption {
                    value: (*value).into(),
                    label: (*label).into(),
                })
                .collect(),
        },
        Value::from(default),
    )
}

/// A live colour, `#rrggbb` or, with `alpha`, `#rrggbbaa`.
pub(crate) fn color(id: &str, label: &str, alpha: bool, default: &str) -> ParamDescriptor {
    param(id, label, ParamType::Color { alpha }, Value::from(default))
}

/// A filter's settings, read as shader values. A missing or mistyped setting reads as
/// the default given; the graph has already checked every range.
pub(crate) struct Read<'a>(pub &'a Settings);

impl Read<'_> {
    pub(crate) fn f32(&self, id: &str, default: f32) -> f32 {
        self.0
            .0
            .get(id)
            .and_then(Value::as_f64)
            .map_or(default, |v| v as f32)
    }

    pub(crate) fn i64(&self, id: &str, default: i64) -> i64 {
        self.0.0.get(id).and_then(Value::as_i64).unwrap_or(default)
    }

    pub(crate) fn bool(&self, id: &str, default: bool) -> bool {
        self.0.0.get(id).and_then(Value::as_bool).unwrap_or(default)
    }

    /// An enum setting's value.
    pub(crate) fn choice<'s>(&'s self, id: &str, default: &'s str) -> &'s str {
        self.0.0.get(id).and_then(Value::as_str).unwrap_or(default)
    }

    /// A colour setting as straight-alpha linear RGBA, or `default` (also linear) when
    /// it is missing or not `#rrggbb` / `#rrggbbaa`.
    pub(crate) fn color_linear(&self, id: &str, default: [f32; 4]) -> [f32; 4] {
        self.0
            .0
            .get(id)
            .and_then(Value::as_str)
            .and_then(parse_hex)
            .map_or(default, |[r, g, b, a]| {
                let channel = |c: u8| srgb_channel_to_linear(f32::from(c) / 255.0);
                [channel(r), channel(g), channel(b), f32::from(a) / 255.0]
            })
    }
}

/// `#rrggbb` or `#rrggbbaa` as bytes; alpha is 255 when absent.
fn parse_hex(text: &str) -> Option<[u8; 4]> {
    let hex = text.strip_prefix('#')?;
    if !hex.is_ascii() || (hex.len() != 6 && hex.len() != 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let a = if hex.len() == 8 { byte(6)? } else { 255 };
    Some([byte(0)?, byte(2)?, byte(4)?, a])
}

/// Bytes for a WGSL uniform struct, written field by field in declaration order. Each
/// field is aligned as WGSL's uniform layout requires (`f32`, `u32`, `i32`: 4;
/// `vec2`: 8; `vec4`: 16), and the struct is padded to a multiple of 16. `vec3` is not
/// offered: use a `vec4` and ignore `w`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Uniforms(Vec<u8>);

impl Uniforms {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn align(&mut self, to: usize) {
        while !self.0.len().is_multiple_of(to) {
            self.0.push(0);
        }
    }

    pub(crate) fn f32(mut self, value: f32) -> Self {
        self.align(4);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn u32(mut self, value: u32) -> Self {
        self.align(4);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn i32(mut self, value: i32) -> Self {
        self.align(4);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn vec2(mut self, value: [f32; 2]) -> Self {
        self.align(8);
        for v in value {
            self.0.extend_from_slice(&v.to_le_bytes());
        }
        self
    }

    pub(crate) fn vec4(mut self, value: [f32; 4]) -> Self {
        self.align(16);
        for v in value {
            self.0.extend_from_slice(&v.to_le_bytes());
        }
        self
    }

    /// The bytes, padded to a multiple of 16 and never empty.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.0.is_empty() {
            self.0.resize(16, 0);
        }
        self.align(16);
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{Read, Uniforms, parse_hex};
    use jstream_protocol::Settings;
    use serde_json::json;

    #[test]
    fn uniforms_follow_wgsl_alignment() {
        let bytes = Uniforms::new()
            .f32(1.0)
            .vec2([2.0, 3.0])
            .vec4([4.0; 4])
            .finish();
        // f32 at 0, vec2 at 8, vec4 at 16, struct size 32.
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[8..12], &2.0f32.to_le_bytes());
        assert_eq!(&bytes[16..20], &4.0f32.to_le_bytes());
        assert_eq!(Uniforms::new().finish().len(), 16);
    }

    #[test]
    fn colors_parse_with_and_without_alpha() {
        assert_eq!(parse_hex("#ff0080"), Some([255, 0, 128, 255]));
        assert_eq!(parse_hex("#00000080"), Some([0, 0, 0, 128]));
        assert_eq!(parse_hex("ff0080"), None);
        assert_eq!(parse_hex("#ff00"), None);
        let settings = Settings([("c".to_owned(), json!("#ffffff80"))].into_iter().collect());
        let c = Read(&settings).color_linear("c", [0.0; 4]);
        assert!((c[0] - 1.0).abs() < 1e-6 && (c[3] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(Read(&settings).color_linear("missing", [0.5; 4]), [0.5; 4]);
    }
}
