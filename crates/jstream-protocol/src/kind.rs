//! Kind descriptors: what the engine reports about each source or filter kind, so every
//! client can render and validate settings without code per kind (ADR-0025,
//! docs/adr/0025-typed-settings-kind-descriptors.md).
//!
//! The protocol defines these shapes once and never a type per kind. The checks on one
//! value ([`ParamDescriptor::check`]) are pure functions here, so the engine's graph and
//! any other implementation share them.

use crate::query::CaptureTargetKind;
use crate::settings::{AssetRoot, Reference};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// What a kind is: a source kind, or a filter kind for video or audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KindCategory {
    /// A source kind (`source.create` with `builtin { kind }`).
    Source,
    /// A video filter kind.
    VideoFilter,
    /// An audio filter kind.
    AudioFilter,
    /// A transition kind, which a transition preset names (ADR-0037).
    Transition,
    /// A category this version of the protocol doesn't know. Never sent; produced only
    /// when parsing a `kind.list` from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// One kind in a `kind.list` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KindSummary {
    /// The kind's ID: a bare name for a built-in kind (`image`), `<pack>/<effect>` for a
    /// pack kind.
    pub kind_id: String,
    /// What the kind is.
    pub category: KindCategory,
    /// Display name.
    pub name: String,
    /// The descriptor's version. Stored settings record the version they were written
    /// with.
    pub version: u32,
    /// An effect-pack kind the engine quarantined after a GPU device loss was blamed on
    /// it (0.62, `gpu.device_lost`): its filters are bypassed until a person re-enables
    /// it (`effect.enable`). Omitted when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quarantined: bool,
}

/// Everything a client needs to show and edit one kind's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KindDescriptor {
    /// The kind's ID, as in [`KindSummary::kind_id`].
    pub kind_id: String,
    /// What the kind is.
    pub category: KindCategory,
    /// Display name.
    pub name: String,
    /// The descriptor's version.
    pub version: u32,
    /// Whether the kind produces video.
    pub video: bool,
    /// Whether the kind produces audio.
    pub audio: bool,
    /// The parameters, in display order. Their IDs are unique within the kind.
    pub params: Vec<ParamDescriptor>,
    /// For a `video_filter` kind that draws outside its input (a drop shadow, a glow, an
    /// outline): the most it can draw beyond each edge, in pixels, at any settings
    /// (ADR-0065 §5, since 0.43). An item's effects layer is padded by the sum over its
    /// enabled effects, so a live edit never resizes it. On a source's or a scene's chain
    /// it is ignored, since the output there is the input's size. Omitted for none,
    /// and for every other category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_outset_px: Option<u32>,
}

impl KindDescriptor {
    /// The summary `kind.list` reports.
    pub fn summary(&self) -> KindSummary {
        KindSummary {
            kind_id: self.kind_id.clone(),
            category: self.category,
            name: self.name.clone(),
            version: self.version,
            quarantined: false,
        }
    }

    /// The parameter with this ID.
    pub fn param(&self, id: &str) -> Option<&ParamDescriptor> {
        self.params.iter().find(|p| p.id == id)
    }

    /// A JSON Schema (draft 2020-12) of a complete settings map of this kind, generated
    /// from the parameters. `kind.describe` returns it beside the descriptor.
    ///
    /// It describes the stored map. A `source.set_settings` patch may also send `null`
    /// for any parameter, to reset it to its default.
    pub fn settings_schema(&self) -> Value {
        let properties: Map<String, Value> = self
            .params
            .iter()
            .map(|p| (p.id.clone(), p.value_schema()))
            .collect();
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": format!("{} settings", self.name),
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        })
    }
}

/// One parameter of a kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParamDescriptor {
    /// Unique within the kind, and stable across versions.
    pub id: String,
    /// The value's type and its constraints, flattened into this object under the `type`
    /// tag.
    #[serde(flatten)]
    pub ty: ParamType,
    /// The value a new source gets when it doesn't set this parameter.
    pub default: Value,
    /// Display label.
    pub label: String,
    /// A longer explanation for UIs and agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// UI hint: the group to show it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// UI hint: shown only among advanced settings.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advanced: bool,
    /// UI hint: shown only while another parameter has a given value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<VisibleIf>,
    /// Whether a change applies without restarting the source or filter (a colour), or
    /// restarts it (a capture device). `live.set` may override a live parameter during a
    /// gesture (ADR-0032).
    #[serde(default)]
    pub live: bool,
}

/// A UI hint: show a parameter only while parameter `param` equals `equals`. Hints never
/// affect validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VisibleIf {
    /// The parameter whose value decides.
    pub param: String,
    /// The value it must have.
    pub equals: Value,
}

/// A parameter's type and constraints.
///
/// Serialized with an internal `type` tag. A client that meets a type it doesn't know
/// parses it as [`ParamType::Unknown`] and shows the parameter read-only, so adding a
/// type is a minor protocol change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ParamType {
    /// `true` or `false`.
    Bool,
    /// A whole number.
    Int {
        /// Smallest allowed value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        /// Largest allowed value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
        /// UI hint: the increment a control steps by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<i64>,
        /// Display unit (`px`, `ms`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    /// A finite number.
    Float {
        /// Smallest allowed value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Largest allowed value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// UI hint: the increment a control steps by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        /// Display unit (`dB`, `%`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    /// One of a fixed set of strings.
    Enum {
        /// The allowed values, in display order.
        options: Vec<EnumOption>,
    },
    /// A colour: `#rrggbb`, or `#rrggbbaa` when `alpha` is true.
    Color {
        /// Whether the value may carry an alpha byte.
        alpha: bool,
    },
    /// Text.
    String {
        /// UI hint: shown as multi-line text.
        #[serde(default)]
        multiline: bool,
        /// Longest allowed value, in characters.
        max_len: u32,
    },
    /// A file reference (`{"type":"asset","root":…,"path":…}`) or `null`.
    Asset {
        /// The accepted file extensions, lower case without the dot (`png`). Empty
        /// accepts any file.
        #[serde(default)]
        accept: Vec<String>,
    },
    /// A scene reference (`{"type":"scene","scene_id":…}`) or `null`.
    Scene,
    /// A source reference (`{"type":"source","source_id":…}`) or `null`.
    Source {
        /// Accept only sources that produce this. Omitted accepts any source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media: Option<Media>,
    },
    /// A bus reference (`{"type":"bus","bus_id":…}`) or `null`. Since 0.24.
    Bus,
    /// The ID of a display or window, as `capture.targets` lists it, so a UI can offer
    /// the targets of that kind to pick from: a string, empty for the kind's default.
    /// Since 0.50.
    CaptureTarget {
        /// The kind of target it names.
        kind: CaptureTargetKind,
        /// Longest allowed value, in characters.
        max_len: u32,
    },
    /// A type this version of the protocol doesn't know. Never sent; produced only when
    /// parsing a descriptor from a newer server.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// One option of an [`ParamType::Enum`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnumOption {
    /// The value stored in settings.
    pub value: String,
    /// Display label.
    pub label: String,
}

/// What a source produces, for restricting a source reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Media {
    /// Pictures.
    Video,
    /// Sound.
    Audio,
}

impl ParamDescriptor {
    /// Whether the parameter holds a reference (asset, scene, source or bus), and so
    /// accepts `null` for "no reference".
    pub fn is_reference(&self) -> bool {
        matches!(
            self.ty,
            ParamType::Asset { .. } | ParamType::Scene | ParamType::Source { .. } | ParamType::Bus
        )
    }

    /// Checks `value` against this parameter's type and constraints, reading nothing
    /// else: whether a referenced scene, source or bus exists (and what it produces) is the
    /// graph's check. `null` passes only for a reference parameter. Values are never
    /// clamped. The error is the reason, for an `invalid_argument` that names the
    /// parameter.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        match &self.ty {
            ParamType::Bool => value
                .is_boolean()
                .then_some(())
                .ok_or_else(|| "must be true or false".to_owned()),
            ParamType::Int { min, max, .. } => {
                let Some(n) = value.as_i64() else {
                    return Err("must be a whole number".into());
                };
                check_range(n, *min, *max)
            }
            ParamType::Float { min, max, .. } => {
                let Some(n) = value.as_f64() else {
                    return Err("must be a number".into());
                };
                check_range(n, *min, *max)
            }
            ParamType::Enum { options } => {
                let Some(s) = value.as_str() else {
                    return Err("must be a string".into());
                };
                if options.iter().any(|o| o.value == s) {
                    Ok(())
                } else {
                    let names: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
                    Err(format!("must be one of {}", names.join(", ")))
                }
            }
            ParamType::Color { alpha } => {
                let ok = value.as_str().is_some_and(|s| is_color(s, *alpha));
                ok.then_some(()).ok_or_else(|| {
                    if *alpha {
                        "must be a colour as `#rrggbb` or `#rrggbbaa`".to_owned()
                    } else {
                        "must be a colour as `#rrggbb`".to_owned()
                    }
                })
            }
            ParamType::String { max_len, .. } | ParamType::CaptureTarget { max_len, .. } => {
                let Some(s) = value.as_str() else {
                    return Err("must be a string".into());
                };
                let len = s.chars().count();
                if u64::try_from(len).unwrap_or(u64::MAX) > u64::from(*max_len) {
                    return Err(format!("must be at most {max_len} characters long"));
                }
                Ok(())
            }
            ParamType::Asset { accept } => match reference(value, "asset")? {
                None => Ok(()),
                Some(Reference::Asset { root, path }) => {
                    check_asset_path(root, &path)?;
                    check_extension(&path, accept)
                }
                Some(_) => Err("must be an asset reference or null".into()),
            },
            ParamType::Scene => match reference(value, "scene")? {
                None | Some(Reference::Scene { .. }) => Ok(()),
                Some(_) => Err("must be a scene reference or null".into()),
            },
            ParamType::Source { .. } => match reference(value, "source")? {
                None | Some(Reference::Source { .. }) => Ok(()),
                Some(_) => Err("must be a source reference or null".into()),
            },
            ParamType::Bus => match reference(value, "bus")? {
                None | Some(Reference::Bus { .. }) => Ok(()),
                Some(_) => Err("must be a bus reference or null".into()),
            },
            ParamType::Unknown => Err("has a type this engine can't check".into()),
        }
    }

    /// The JSON Schema of one value of this parameter, with its label, description and
    /// default.
    fn value_schema(&self) -> Value {
        let null_or = |schema: Value| json!({"anyOf": [{"type": "null"}, schema]});
        let mut schema = match &self.ty {
            ParamType::Bool => json!({"type": "boolean"}),
            ParamType::Int { min, max, .. } => {
                bounded("integer", min.map(Value::from), max.map(Value::from))
            }
            ParamType::Float { min, max, .. } => {
                bounded("number", min.map(Value::from), max.map(Value::from))
            }
            ParamType::Enum { options } => {
                json!({"enum": options.iter().map(|o| o.value.clone()).collect::<Vec<_>>()})
            }
            ParamType::Color { alpha } => {
                let pattern = if *alpha {
                    "^#[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$"
                } else {
                    "^#[0-9A-Fa-f]{6}$"
                };
                json!({"type": "string", "pattern": pattern})
            }
            ParamType::String { max_len, .. } | ParamType::CaptureTarget { max_len, .. } => {
                json!({"type": "string", "maxLength": max_len})
            }
            ParamType::Asset { .. } => null_or(json!({
                "type": "object",
                "properties": {
                    "type": {"const": "asset"},
                    "root": {"enum": ["collection", "app", "absolute"]},
                    "path": {"type": "string"},
                },
                "required": ["type", "root", "path"],
            })),
            ParamType::Scene => null_or(json!({
                "type": "object",
                "properties": {"type": {"const": "scene"}, "scene_id": {"type": "string"}},
                "required": ["type", "scene_id"],
            })),
            ParamType::Source { .. } => null_or(json!({
                "type": "object",
                "properties": {"type": {"const": "source"}, "source_id": {"type": "string"}},
                "required": ["type", "source_id"],
            })),
            ParamType::Bus => null_or(json!({
                "type": "object",
                "properties": {"type": {"const": "bus"}, "bus_id": {"type": "string"}},
                "required": ["type", "bus_id"],
            })),
            ParamType::Unknown => json!({}),
        };
        if let Some(object) = schema.as_object_mut() {
            object.insert("title".into(), Value::from(self.label.clone()));
            if let Some(description) = &self.description {
                object.insert("description".into(), Value::from(description.clone()));
            }
            object.insert("default".into(), self.default.clone());
        }
        schema
    }
}

fn bounded(ty: &str, min: Option<Value>, max: Option<Value>) -> Value {
    let mut schema = json!({ "type": ty });
    if let Some(object) = schema.as_object_mut() {
        if let Some(min) = min {
            object.insert("minimum".into(), min);
        }
        if let Some(max) = max {
            object.insert("maximum".into(), max);
        }
    }
    schema
}

fn check_range<T: PartialOrd + std::fmt::Display + Copy>(
    n: T,
    min: Option<T>,
    max: Option<T>,
) -> Result<(), String> {
    match (min, max) {
        (Some(lo), Some(hi)) if n < lo || n > hi => Err(format!("must be from {lo} to {hi}")),
        (Some(lo), None) if n < lo => Err(format!("must be at least {lo}")),
        (None, Some(hi)) if n > hi => Err(format!("must be at most {hi}")),
        _ => Ok(()),
    }
}

fn is_color(s: &str, alpha: bool) -> bool {
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 6 || (alpha && hex.len() == 8)) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `None` for `null`, the reference for an object tagged `want`, else an error.
fn reference(value: &Value, want: &str) -> Result<Option<Reference>, String> {
    if value.is_null() {
        return Ok(None);
    }
    Reference::from_value(value)
        .map(Some)
        .ok_or_else(|| format!("must be a {want} reference or null"))
}

/// Whether Win32 opens the path component `name` as a DOS device rather than a file.
///
/// The stem (the name before its first `.`, without trailing spaces, compared without
/// regard to ASCII case) is `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `COM¹`–`COM³`,
/// `LPT0`–`LPT9`, `LPT¹`–`LPT³`, `CONIN$` or `CONOUT$`. So `nul`, `NUL `, `nul.`,
/// `Con.png` and `CON .png` are devices, and `CONSOLE.png`, `nul1.png` and `COM10.png`
/// are not. Only the last component matters, under any root: Win32 opens a device named
/// there in any folder (`C:\x\CON`, or `CON.png` joined to a root folder), while a
/// device name earlier in the path fails to open, and on other systems it is an
/// ordinary folder name.
fn is_dos_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => ["COM", "LPT"].iter().any(|port| {
            upper.strip_prefix(port).is_some_and(|n| {
                matches!(n.as_bytes(), [b'0'..=b'9']) || matches!(n, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        }),
    }
}

/// The path rules of an asset reference under `root`.
///
/// Under `collection` and `app` the path is relative: `/` separators, and no empty or
/// `..` segment, drive letter, leading `/`, backslash or `:`. Under `absolute` it must be
/// absolute (`/…`, `\…`, `C:/…` or `C:\…`) and still has no `..` segment. It must not
/// start with two separators, which on Windows name a UNC share (`\\host\share\…`) or a
/// device (`\\.\…`, `\\?\…`): opening one would send the engine's credentials to that
/// host. Nor may it contain `:` other than a drive letter's, or `?` or `*`, which
/// Win32 never allows in a file name and which name an NT object path (`\??\UNC\…`,
/// `\??\GLOBALROOT\Device\…`). Under every root, the last component must not name a
/// DOS device, which Win32 opens in place of a file in any folder: a stem (the name
/// before its first `.`, without trailing spaces, in any ASCII case) of `CON`, `PRN`,
/// `AUX`, `NUL`, `COM0`–`COM9`, `COM¹`–`COM³`, `LPT0`–`LPT9`, `LPT¹`–`LPT³`, `CONIN$` or
/// `CONOUT$`.
pub fn check_asset_path(root: AssetRoot, path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("the asset path must not be empty".into());
    }
    let segments = || path.split(['/', '\\']);
    if segments().any(|s| s == "..") {
        return Err("the asset path must not contain a `..` segment".into());
    }
    let drive =
        path.len() >= 2 && path.as_bytes()[1] == b':' && path.as_bytes()[0].is_ascii_alphabetic();
    match root {
        AssetRoot::Absolute => {
            let rest = if drive { &path[2..] } else { path };
            let separator = |c: char| c == '/' || c == '\\';
            if !rest.starts_with(separator) {
                Err("an `absolute` asset path must be absolute".into())
            } else if rest[1..].starts_with(separator) {
                Err("an `absolute` asset path must not start with two separators".into())
            } else if rest.contains(':') {
                // On Windows `C:\a.png:s` names an NTFS alternate data stream of `a.png`.
                Err("an `absolute` asset path must not contain `:` but a drive letter's".into())
            } else if rest.contains(['?', '*']) {
                // Win32 never allows `?` or `*` in a file name; `\??\…` is an NT object
                // path (`\??\UNC\host\…`, `\??\GLOBALROOT\Device\…`).
                Err("an `absolute` asset path must not contain `?` or `*`".into())
            } else if segments().next_back().is_some_and(is_dos_device_name) {
                Err("an `absolute` asset path must not end in a DOS device name".into())
            } else {
                Ok(())
            }
        }
        _ => {
            if path.contains('\\') {
                Err("a relative asset path uses `/` separators".into())
            } else if path.starts_with('/') || drive {
                Err("a relative asset path must not start with `/` or a drive letter".into())
            } else if path.contains(':') {
                // On Windows `a.png:s` names an NTFS alternate data stream of `a.png`.
                Err("a relative asset path must not contain `:`".into())
            } else if segments().any(str::is_empty) {
                Err("a relative asset path must not have an empty segment".into())
            } else if segments().next_back().is_some_and(is_dos_device_name) {
                // Joined to the root folder, `CON.png` still opens the device on Windows.
                Err("a relative asset path must not end in a DOS device name".into())
            } else {
                Ok(())
            }
        }
    }
}

fn check_extension(path: &str, accept: &[String]) -> Result<(), String> {
    if accept.is_empty() {
        return Ok(());
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if ext.is_some_and(|e| accept.contains(&e)) {
        Ok(())
    } else {
        Err(format!("the file must be one of: {}", accept.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EnumOption, KindCategory, KindDescriptor, KindSummary, ParamDescriptor, ParamType,
    };
    use crate::query::CaptureTargetKind;
    use crate::settings::AssetRoot;
    use serde_json::{Value, json};

    fn param(id: &str, ty: ParamType, default: Value) -> ParamDescriptor {
        ParamDescriptor {
            id: id.into(),
            ty,
            default,
            label: id.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: true,
        }
    }

    #[test]
    fn param_type_is_flattened_under_the_type_tag() {
        let p = param(
            "size",
            ParamType::Int {
                min: Some(1),
                max: Some(8),
                step: None,
                unit: Some("px".into()),
            },
            json!(4),
        );
        let wire = serde_json::to_value(&p).expect("serializes");
        assert_eq!(
            wire,
            json!({"id": "size", "type": "int", "min": 1, "max": 8, "unit": "px",
                   "default": 4, "label": "size", "live": true})
        );
        assert_eq!(
            serde_json::from_value::<ParamDescriptor>(wire).ok(),
            Some(p)
        );
    }

    #[test]
    fn unknown_param_types_from_a_newer_server_parse() {
        let parsed = serde_json::from_value::<ParamDescriptor>(json!({
            "id": "m", "type": "matrix", "rows": 3, "default": null, "label": "M"
        }))
        .expect("parses");
        assert_eq!(parsed.ty, ParamType::Unknown);
        assert_eq!(parsed.id, "m");
    }

    #[test]
    fn unknown_kind_categories_from_a_newer_server_parse() {
        let parsed = serde_json::from_value::<KindSummary>(json!({
            "kind_id": "x", "category": "midi_filter", "name": "X", "version": 1
        }))
        .expect("parses");
        assert_eq!(parsed.category, KindCategory::Unknown);
    }

    #[test]
    fn values_are_checked_not_clamped() {
        let int = param(
            "n",
            ParamType::Int {
                min: Some(0),
                max: Some(10),
                step: None,
                unit: None,
            },
            json!(0),
        );
        assert!(int.check(&json!(10)).is_ok());
        assert_eq!(int.check(&json!(11)), Err("must be from 0 to 10".into()));
        assert!(int.check(&json!(1.5)).is_err());
        assert!(int.check(&json!(null)).is_err());

        let color = param("c", ParamType::Color { alpha: false }, json!("#000000"));
        assert!(color.check(&json!("#a0B1c2")).is_ok());
        assert!(color.check(&json!("#a0B1c2ff")).is_err());
        assert!(color.check(&json!("red")).is_err());

        let choice = param(
            "e",
            ParamType::Enum {
                options: vec![EnumOption {
                    value: "a".into(),
                    label: "A".into(),
                }],
            },
            json!("a"),
        );
        assert_eq!(choice.check(&json!("b")), Err("must be one of a".into()));

        let text = param(
            "t",
            ParamType::String {
                multiline: false,
                max_len: 3,
            },
            json!(""),
        );
        assert!(text.check(&json!("héé")).is_ok());
        assert!(text.check(&json!("four")).is_err());
    }

    /// A capture target is a string on the wire and in settings, named by its kind, and
    /// checked as text: whether the target exists is the engine's business.
    #[test]
    fn a_capture_target_is_a_bounded_string_of_one_kind() {
        let window = param(
            "target",
            ParamType::CaptureTarget {
                kind: CaptureTargetKind::Window,
                max_len: 4,
            },
            json!(""),
        );
        let wire = serde_json::to_value(&window).expect("serializes");
        assert_eq!(wire["type"], "capture_target");
        assert_eq!(wire["kind"], "window");
        assert_eq!(wire["max_len"], 4);
        assert_eq!(
            serde_json::from_value::<ParamDescriptor>(wire).ok(),
            Some(window.clone())
        );
        assert!(window.check(&json!("")).is_ok());
        assert!(window.check(&json!("w:42")).is_ok());
        assert!(window.check(&json!("w:420")).is_err());
        assert!(window.check(&json!(42)).is_err());
        assert!(window.check(&Value::Null).is_err());
        assert!(!window.is_reference());
        // A target kind from a newer server still parses.
        let newer = serde_json::from_value::<ParamDescriptor>(json!({
            "id": "app", "type": "capture_target", "kind": "application", "max_len": 8,
            "default": "", "label": "App"
        }))
        .expect("parses");
        assert_eq!(
            newer.ty,
            ParamType::CaptureTarget {
                kind: CaptureTargetKind::Unknown,
                max_len: 8
            }
        );
        assert_eq!(
            window.value_schema(),
            json!({"type": "string", "maxLength": 4, "title": "target", "default": ""})
        );
    }

    #[test]
    fn references_accept_null_and_only_their_own_tag() {
        let scene = param("mask", ParamType::Scene, Value::Null);
        assert!(scene.is_reference());
        assert!(scene.check(&Value::Null).is_ok());
        assert!(
            scene
                .check(&json!({"type": "scene", "scene_id": "s"}))
                .is_ok()
        );
        assert!(
            scene
                .check(&json!({"type": "source", "source_id": "s"}))
                .is_err()
        );
        assert!(scene.check(&json!("s")).is_err());

        let bus = param("audio", ParamType::Bus, Value::Null);
        assert!(bus.is_reference());
        assert!(bus.check(&Value::Null).is_ok());
        assert!(bus.check(&json!({"type": "bus", "bus_id": "vod"})).is_ok());
        assert!(
            bus.check(&json!({"type": "source", "source_id": "vod"}))
                .is_err()
        );
        assert!(bus.check(&json!("vod")).is_err());

        let file = param(
            "file",
            ParamType::Asset {
                accept: vec!["png".into()],
            },
            Value::Null,
        );
        let asset = |root: &str, path: &str| json!({"type": "asset", "root": root, "path": path});
        assert!(file.check(&asset("collection", "img/a.PNG")).is_ok());
        assert!(file.check(&asset("absolute", "C:\\img\\a.png")).is_ok());
        assert!(file.check(&asset("collection", "img/a.jpg")).is_err());
        for bad in [
            "../a.png",
            "/a.png",
            "C:/a.png",
            "a\\b.png",
            "a//b.png",
            "",
            "img/a.png:stream",
            "a:b/c.png",
        ] {
            assert!(file.check(&asset("collection", bad)).is_err(), "{bad}");
        }
        assert!(file.check(&asset("absolute", "img/a.png")).is_err());
        assert!(super::check_asset_path(AssetRoot::App, "x/y.png").is_ok());
    }

    /// Issue #403: under `collection` and `app`, a relative path whose last component
    /// names a DOS device is refused; a device name earlier in the path is not.
    #[test]
    fn relative_asset_paths_refuse_a_dos_device_name_last() {
        for root in [AssetRoot::Collection, AssetRoot::App] {
            let check = |path: &str| super::check_asset_path(root, path);
            for good in [
                "CONSOLE.png",
                "media/nul1.png",
                "COM10.png",
                "LPT.png",
                "COM\u{b9}0.png",
                "icon.png",
                "a.con",
                "con/a.png",
                "media/AUX/x/a.png",
            ] {
                assert!(check(good).is_ok(), "{root:?} {good}");
            }
            for bad in [
                "CON",
                "CON.png",
                "media/nul",
                "x/Con.png",
                "aux.tar.gz",
                "prn.",
                "NUL ",
                "CON .png",
                "com1.png",
                "img/COM9",
                "com0.png",
                "LPT1.png",
                "lpt9",
                "COM\u{b9}.png",
                "LPT\u{b3}",
                "CONIN$",
                "a/conout$.png",
            ] {
                assert!(check(bad).is_err(), "{root:?} {bad}");
            }
        }
    }

    /// Issues #345 and #384: UNC, device, stream and DOS device paths are refused under
    /// `absolute`.
    #[test]
    fn absolute_asset_paths_refuse_unc_device_and_stream_forms() {
        let check = |path: &str| super::check_asset_path(AssetRoot::Absolute, path);
        for good in [
            "/home/a.png",
            "\\home\\a.png",
            "C:\\a.png",
            "C:/a.png",
            "c:\\img/a.png",
            // Near misses of a DOS device name, and device names before the last component.
            "C:\\x\\CONSOLE.png",
            "C:\\x\\nul1.png",
            "C:\\x\\COM10.png",
            "C:\\x\\LPT.png",
            "C:\\x\\COM\u{b9}0.png",
            "C:\\x\\icon.png",
            "C:\\x\\a.con",
            "C:\\CON\\a.png",
            "/mnt/aux/a.png",
        ] {
            assert!(check(good).is_ok(), "{good}");
        }
        for bad in [
            "\\\\host\\share\\a.png",
            "//host/share/a.png",
            "\\/host/share/a.png",
            "/\\host\\share\\a.png",
            "\\\\.\\pipe\\x.png",
            "\\\\?\\C:\\a.png",
            "\\\\?\\UNC\\host\\share\\a.png",
            "//./pipe/x.png",
            "C:\\\\host\\a.png",
            "C://a.png",
            "C:a.png",
            "C:\\a.png:stream",
            "/home/a:b.png",
            "\\C:\\a.png",
            "\\??\\UNC\\attacker.example\\share\\a.png",
            "\\??\\GLOBALROOT\\Device\\NamedPipe\\x",
            "/img/a?.png",
            "/img/*.png",
            // Issue #384: a DOS device name as the last component.
            "C:\\x\\CON",
            "\\NUL",
            "/x/nul",
            "C:\\x\\Con.png",
            "C:\\x\\aux.tar.gz",
            "C:\\x\\prn.",
            "C:\\x\\NUL ",
            "C:\\x\\CON .png",
            "C:\\x\\com1.png",
            "C:\\x\\COM9",
            "C:\\x\\com0.png",
            "C:\\x\\LPT1.png",
            "C:\\x\\lpt9",
            "C:\\x\\COM\u{b9}.png",
            "C:\\x\\LPT\u{b3}",
            "C:\\x\\CONIN$",
            "C:\\x\\conout$.png",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn settings_schema_lists_every_parameter() {
        let kind = KindDescriptor {
            kind_id: "color".into(),
            category: KindCategory::Source,
            name: "Colour".into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params: vec![
                param(
                    "color",
                    ParamType::Color { alpha: true },
                    json!("#000000ff"),
                ),
                param("mask", ParamType::Scene, Value::Null),
            ],
        };
        let schema = kind.settings_schema();
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["color"]["default"], json!("#000000ff"));
        assert_eq!(
            schema["properties"]["mask"]["anyOf"][0],
            json!({"type": "null"})
        );
    }
}
