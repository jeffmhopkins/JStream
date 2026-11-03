//! `effect.toml`: the pack's name and its effects, each with a shader file, a version, any
//! parameter renames, and its parameters in descriptor form (ADR-0025 › Effect packs).
//!
//! ```toml
//! [pack]
//! id = "retro"
//! name = "Retro"
//!
//! [[effect]]
//! id = "scanlines"
//! name = "Scanlines"
//! version = 2
//! shader = "scanlines.wgsl"
//! renames = { amount = "strength" }  # settings written by version 1 call it `amount`
//!
//! [[effect.param]]
//! id = "strength"
//! label = "Strength"
//! type = "float"
//! min = 0.0
//! max = 1.0
//! default = 0.5
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jstream_protocol::{EnumOption, ParamDescriptor, ParamType};
use serde::Deserialize;
use serde_json::Value;

use crate::error::PackError;

/// The longest pack, effect or parameter ID.
pub const MAX_ID_LEN: usize = 32;

/// The most parameters one effect may declare.
pub const MAX_PARAMS: usize = 64;

/// The most effects and shader sources one pack may declare, together.
pub const MAX_EFFECTS: usize = 64;

/// The smallest and largest `width` and `height` of a shader source.
pub const SOURCE_SIDE: std::ops::RangeInclusive<u32> = 16..=4096;

/// The most images a shader source names.
pub const MAX_IMAGES: usize = 12;

/// Setting IDs every shader source has, so a pack can't declare them.
pub(crate) const SOURCE_SETTINGS: [&str; 4] = ["width", "height", "audio", "audio_source"];

/// The name a shader source reads its audio as.
pub(crate) const AUDIO_INPUT: &str = "audio";

/// The manifest as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub pack: PackSection,
    #[serde(default, rename = "effect")]
    pub effects: Vec<EffectSection>,
    /// Shader sources (ADR-0093).
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceSection>,
}

/// A shader source (ADR-0093): a program of passes that makes a picture of its own.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceSection {
    pub id: String,
    pub name: String,
    pub version: u32,
    /// The `width` setting's default.
    #[serde(default = "default_width")]
    pub width: u32,
    /// The `height` setting's default.
    #[serde(default = "default_height")]
    pub height: u32,
    /// WGSL every pass gets between the generated header and its own file.
    #[serde(default)]
    pub common: Option<String>,
    /// Whether the source hears audio: its `audio` and `audio_source` settings and its
    /// `audio` input.
    #[serde(default)]
    pub audio: bool,
    /// The pack's images it reads, by the name a pass reads them as.
    #[serde(default)]
    pub images: BTreeMap<String, String>,
    #[serde(default)]
    pub renames: BTreeMap<String, String>,
    #[serde(default, rename = "param")]
    pub params: Vec<ParamSection>,
    #[serde(default, rename = "pass")]
    pub passes: Vec<PassSection>,
}

fn default_width() -> u32 {
    1920
}

fn default_height() -> u32 {
    1080
}

/// One pass of a shader source.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PassSection {
    pub id: String,
    pub shader: String,
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// The names it reads: passes, images, `scene` parameters and `audio`.
    #[serde(default)]
    pub inputs: Vec<String>,
}

fn default_scale() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackSection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectSection {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub shader: String,
    #[serde(default)]
    pub renames: BTreeMap<String, String>,
    #[serde(default, rename = "param")]
    pub params: Vec<ParamSection>,
}

/// One parameter. Its type's fields sit beside the common ones, as in a descriptor, so
/// it is read through a table: serde can't refuse unknown fields across a flatten.
#[derive(Debug, Deserialize)]
#[serde(try_from = "toml::Table")]
pub(crate) struct ParamSection {
    pub id: String,
    pub label: String,
    pub ty: ParamKind,
    pub description: Option<String>,
    pub group: Option<String>,
    pub advanced: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Common {
    id: String,
    label: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    advanced: bool,
}

impl TryFrom<toml::Table> for ParamSection {
    type Error = toml::de::Error;

    fn try_from(mut table: toml::Table) -> Result<Self, Self::Error> {
        let mut common = toml::Table::new();
        for key in ["id", "label", "description", "group", "advanced"] {
            if let Some(value) = table.remove(key) {
                common.insert(key.to_owned(), value);
            }
        }
        let Common {
            id,
            label,
            description,
            group,
            advanced,
        } = common.try_into()?;
        let ty = table.try_into()?;
        Ok(Self {
            id,
            label,
            ty,
            description,
            group,
            advanced,
        })
    }
}

/// The parameter types a shader can read. Text, files and references have no uniform
/// form, so a pack can't declare them.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ParamKind {
    Bool {
        default: bool,
    },
    Int {
        default: i64,
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
        #[serde(default)]
        step: Option<i64>,
        #[serde(default)]
        unit: Option<String>,
    },
    Float {
        default: f64,
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
        #[serde(default)]
        step: Option<f64>,
        #[serde(default)]
        unit: Option<String>,
    },
    Enum {
        default: String,
        options: Vec<OptionSection>,
    },
    Color {
        default: String,
        #[serde(default)]
        alpha: bool,
    },
    /// A scene a shader source reads as an image (ADR-0093). Its default is none.
    Scene,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionSection {
    pub value: String,
    pub label: String,
}

/// Parses `text`, read from `path`, and checks every rule that needs no shader.
pub(crate) fn parse(path: &Path, text: &str) -> Result<Manifest, PackError> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| PackError::Syntax {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    check(path, &manifest)?;
    Ok(manifest)
}

fn check(path: &Path, manifest: &Manifest) -> Result<(), PackError> {
    let fail = |at: String, message: String| PackError::Manifest {
        path: path.to_owned(),
        at,
        message,
    };
    let pack = &manifest.pack;
    check_id(&pack.id, false).map_err(|m| fail("pack.id".into(), m))?;
    if pack.id == "clap" {
        // ADR-0037 §2: `clap/<plugin id>` belongs to CLAP plugin kinds.
        return Err(fail(
            "pack.id".into(),
            "`clap` is reserved for CLAP plugin kinds".into(),
        ));
    }
    check_name(&pack.name).map_err(|m| fail("pack.name".into(), m))?;
    if manifest.effects.is_empty() && manifest.sources.is_empty() {
        return Err(fail(
            "effect".into(),
            "a pack declares at least one effect or source".into(),
        ));
    }
    if manifest.effects.len() + manifest.sources.len() > MAX_EFFECTS {
        return Err(fail(
            "effect".into(),
            format!("a pack declares at most {MAX_EFFECTS} effects and sources together"),
        ));
    }
    let mut effect_ids = BTreeSet::new();
    for effect in &manifest.effects {
        let at = |field: &str| format!("effect.{}.{field}", effect.id);
        check_id(&effect.id, false).map_err(|m| fail("effect.id".into(), m))?;
        if !effect_ids.insert(effect.id.as_str()) {
            return Err(fail(at("id"), "another effect has this ID".into()));
        }
        check_name(&effect.name).map_err(|m| fail(at("name"), m))?;
        if effect.version == 0 {
            return Err(fail(at("version"), "versions start at 1".into()));
        }
        check_shader_path(&effect.shader).map_err(|m| fail(at("shader"), m))?;
        if effect.params.len() > MAX_PARAMS {
            return Err(fail(
                at("param"),
                format!("an effect declares at most {MAX_PARAMS} parameters"),
            ));
        }
        let mut param_ids = BTreeSet::new();
        for param in &effect.params {
            let at = |field: &str| format!("effect.{}.param.{}.{field}", effect.id, param.id);
            check_id(&param.id, true).map_err(|m| fail(at("id"), m))?;
            if !param_ids.insert(param.id.as_str()) {
                return Err(fail(at("id"), "another parameter has this ID".into()));
            }
            check_name(&param.label).map_err(|m| fail(at("label"), m))?;
            if matches!(param.ty, ParamKind::Scene) {
                return Err(fail(
                    at("type"),
                    "an effect has no scene parameters; a shader source reads scenes".into(),
                ));
            }
            check_param(&param.ty).map_err(|(field, m)| fail(at(field), m))?;
            let descriptor = descriptor(param);
            descriptor
                .check(&descriptor.default)
                .map_err(|m| fail(at("default"), format!("the default {m}")))?;
        }
        for (old, new) in &effect.renames {
            let at = format!("effect.{}.renames.{old}", effect.id);
            if param_ids.contains(old.as_str()) {
                return Err(fail(
                    at,
                    "a renamed parameter's old ID must not still be declared".into(),
                ));
            }
            if !param_ids.contains(new.as_str()) {
                return Err(fail(at, format!("`{new}` is not a declared parameter")));
            }
        }
        if !effect.renames.is_empty() && effect.version == 1 {
            return Err(fail(
                at("renames"),
                "a rename comes with a version above 1".into(),
            ));
        }
    }
    for source in &manifest.sources {
        check_source(source, &mut effect_ids).map_err(|(at, m)| fail(at, m))?;
    }
    Ok(())
}

/// Checks one shader source, whose ID shares `ids` with the pack's effects.
/// The names the shader-source prelude and header define, which no pass, image or scene
/// parameter may take: its bindings, entry points, helpers and `audio`'s helpers.
const PRELUDE_NAMES: [&str; 19] = [
    "sys",
    "lin",
    "rep",
    "p",
    "vs",
    "fs",
    "input_alpha",
    "hash12",
    "hash22",
    "noise",
    "fbm",
    "to_premul_linear",
    "unpremul",
    "premul",
    "linear_to_srgb",
    "srgb_to_linear",
    "luma",
    "audio_band",
    "audio_wave",
];

/// What the header appends to an input's name for its texture and helpers.
const INPUT_SUFFIXES: [&str; 4] = ["_tex", "_wrap", "_texel", "_size"];

fn check_source<'a>(
    source: &'a SourceSection,
    ids: &mut BTreeSet<&'a str>,
) -> Result<(), (String, String)> {
    let at = |field: &str| format!("source.{}.{field}", source.id);
    check_id(&source.id, false).map_err(|m| ("source.id".to_owned(), m))?;
    if !ids.insert(source.id.as_str()) {
        return Err((at("id"), "another effect or source has this ID".into()));
    }
    check_name(&source.name).map_err(|m| (at("name"), m))?;
    if source.version == 0 {
        return Err((at("version"), "versions start at 1".into()));
    }
    for (field, side) in [("width", source.width), ("height", source.height)] {
        if !SOURCE_SIDE.contains(&side) {
            return Err((
                at(field),
                format!(
                    "must be {} to {} pixels",
                    SOURCE_SIDE.start(),
                    SOURCE_SIDE.end()
                ),
            ));
        }
    }
    if let Some(common) = &source.common {
        check_shader_path(common).map_err(|m| (at("common"), m))?;
    }
    // Every name a pass can read: passes, images, scene parameters and `audio`.
    let mut names: BTreeSet<&str> = BTreeSet::new();
    let mut fields: Vec<(&str, String)> = Vec::new();
    let mut claim = |name: &'a str, field: String| -> Result<(), (String, String)> {
        check_id(name, true).map_err(|m| (field.clone(), m))?;
        if PRELUDE_NAMES.contains(&name) {
            return Err((
                field,
                format!("`{name}` is a name the shader prelude defines"),
            ));
        }
        fields.push((name, field.clone()));
        if !names.insert(name) {
            return Err((
                field,
                format!("`{name}` already names a pass, an image or a scene parameter"),
            ));
        }
        Ok(())
    };
    if source.audio {
        claim(AUDIO_INPUT, at("audio"))?;
    }
    if source.params.len() > MAX_PARAMS {
        return Err((
            at("param"),
            format!("a source declares at most {MAX_PARAMS} parameters"),
        ));
    }
    let mut param_ids = BTreeSet::new();
    for param in &source.params {
        let at = |field: &str| format!("source.{}.param.{}.{field}", source.id, param.id);
        check_id(&param.id, true).map_err(|m| (at("id"), m))?;
        if SOURCE_SETTINGS.contains(&param.id.as_str()) {
            return Err((
                at("id"),
                "every shader source has this setting already".into(),
            ));
        }
        if !param_ids.insert(param.id.as_str()) {
            return Err((at("id"), "another parameter has this ID".into()));
        }
        check_name(&param.label).map_err(|m| (at("label"), m))?;
        check_param(&param.ty).map_err(|(field, m)| (at(field), m))?;
        let descriptor = descriptor(param);
        descriptor
            .check(&descriptor.default)
            .map_err(|m| (at("default"), format!("the default {m}")))?;
        if matches!(param.ty, ParamKind::Scene) {
            claim(&param.id, at("id"))?;
        }
    }
    if source.images.len() > MAX_IMAGES {
        return Err((
            at("images"),
            format!("a source names at most {MAX_IMAGES} images"),
        ));
    }
    for (name, path) in &source.images {
        let field = format!("source.{}.images.{name}", source.id);
        claim(name, field.clone())?;
        check_image_path(path).map_err(|m| (field, m))?;
    }
    if source.passes.is_empty() || source.passes.len() > jstream_render::PROGRAM_MAX_PASSES {
        return Err((
            at("pass"),
            format!(
                "a source has 1 to {} passes",
                jstream_render::PROGRAM_MAX_PASSES
            ),
        ));
    }
    for pass in &source.passes {
        claim(&pass.id, format!("source.{}.pass.id", source.id))?;
    }
    // Each name `n` also defines `n_tex`, `n_wrap`, `n_texel` and `n_size`.
    for (name, field) in &fields {
        for suffix in INPUT_SUFFIXES {
            if let Some(base) = name.strip_suffix(suffix)
                && names.contains(base)
            {
                return Err((
                    field.clone(),
                    format!("`{name}` is a helper the name `{base}` defines"),
                ));
            }
        }
    }
    let last = source.passes.len() - 1;
    for (n, pass) in source.passes.iter().enumerate() {
        let at = |field: &str| format!("source.{}.pass.{}.{field}", source.id, pass.id);
        check_shader_path(&pass.shader).map_err(|m| (at("shader"), m))?;
        if !(pass.scale.is_finite() && (1.0 / 16.0..=1.0).contains(&pass.scale)) {
            return Err((at("scale"), "must be 1/16 to 1".into()));
        }
        if n == last && pass.scale != 1.0 {
            return Err((
                at("scale"),
                "the last pass is the source's picture, drawn at its whole size".into(),
            ));
        }
        if pass.inputs.len() > jstream_render::PROGRAM_MAX_INPUTS {
            return Err((
                at("inputs"),
                format!(
                    "a pass reads at most {} names",
                    jstream_render::PROGRAM_MAX_INPUTS
                ),
            ));
        }
        let mut read = BTreeSet::new();
        for input in &pass.inputs {
            if !names.contains(input.as_str()) {
                return Err((
                    at("inputs"),
                    format!("`{input}` names no pass, image, scene parameter or audio"),
                ));
            }
            if !read.insert(input.as_str()) {
                return Err((at("inputs"), format!("`{input}` is listed twice")));
            }
        }
    }
    for (old, new) in &source.renames {
        let at = format!("source.{}.renames.{old}", source.id);
        if param_ids.contains(old.as_str()) {
            return Err((
                at,
                "a renamed parameter's old ID must not still be declared".into(),
            ));
        }
        if !param_ids.contains(new.as_str()) {
            return Err((at, format!("`{new}` is not a declared parameter")));
        }
    }
    if !source.renames.is_empty() && source.version == 1 {
        return Err((
            at("renames"),
            "a rename comes with a version above 1".into(),
        ));
    }
    Ok(())
}

/// An image path is relative to the pack folder, like a shader's, and names a PNG or
/// a JPEG.
fn check_image_path(path: &str) -> Result<(), String> {
    let image = Path::new(path).extension().is_some_and(|e| {
        ["png", "jpg", "jpeg"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    });
    if !image {
        return Err("must name a `.png`, `.jpg` or `.jpeg` file".into());
    }
    jstream_protocol::check_asset_path(jstream_protocol::AssetRoot::Collection, path)
}

/// Pack and effect IDs: lower-case ASCII letters, digits and `-`, starting with a letter
/// or digit. Parameter IDs are WGSL field names: lower-case letters, digits and `_`,
/// starting with a letter.
fn check_id(id: &str, param: bool) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(format!("`{id}` must be 1 to {MAX_ID_LEN} characters"));
    }
    let ok = if param {
        id.starts_with(|c: char| c.is_ascii_lowercase())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && !id.contains("__")
    } else {
        id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if ok {
        Ok(())
    } else if param {
        Err(format!(
            "`{id}` must start with a-z and use only a-z, 0-9 and single `_`"
        ))
    } else {
        Err(format!(
            "`{id}` must use only a-z, 0-9 and `-`, and not start with `-`"
        ))
    }
}

fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.chars().count() > 80 {
        return Err("must be 1 to 80 characters".into());
    }
    Ok(())
}

/// A shader path is relative to the pack folder, with `/` separators, and stays in it.
fn check_shader_path(shader: &str) -> Result<(), String> {
    let has_wgsl = Path::new(shader)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wgsl"));
    if !has_wgsl {
        return Err("must name a `.wgsl` file".into());
    }
    jstream_protocol::check_asset_path(jstream_protocol::AssetRoot::Collection, shader)
}

fn check_param(ty: &ParamKind) -> Result<(), (&'static str, String)> {
    match ty {
        ParamKind::Int { min, max, .. } => {
            // A shader reads an `i32`, so the range is bounded and fits one.
            let (Some(min), Some(max)) = (min, max) else {
                return Err((
                    "max",
                    "an integer parameter declares both `min` and `max`".into(),
                ));
            };
            if min > max {
                return Err(("min", "is above `max`".into()));
            }
            for (field, v) in [("min", min), ("max", max)] {
                if i32::try_from(*v).is_err() {
                    return Err((field, "must fit a 32-bit integer".into()));
                }
            }
        }
        ParamKind::Float {
            default,
            min,
            max,
            step,
            ..
        } => {
            for (field, v) in [
                ("default", Some(default)),
                ("min", min.as_ref()),
                ("max", max.as_ref()),
                ("step", step.as_ref()),
            ] {
                if v.is_some_and(|v| !v.is_finite()) {
                    return Err((field, "must be finite".into()));
                }
            }
            if let (Some(min), Some(max)) = (min, max)
                && min > max
            {
                return Err(("min", "is above `max`".into()));
            }
        }
        ParamKind::Enum { options, .. } => {
            if options.is_empty() {
                return Err(("options", "an enum has at least one option".into()));
            }
            let mut seen = BTreeSet::new();
            for o in options {
                if o.value.is_empty() || !seen.insert(o.value.as_str()) {
                    return Err((
                        "options",
                        format!("option `{}` is empty or repeated", o.value),
                    ));
                }
            }
        }
        ParamKind::Bool { .. } | ParamKind::Color { .. } | ParamKind::Scene => {}
    }
    Ok(())
}

/// The descriptor of one declared parameter. Every pack parameter is live: a change is
/// a new uniform value, never a restart.
pub(crate) fn descriptor(param: &ParamSection) -> ParamDescriptor {
    let (ty, default) = match &param.ty {
        ParamKind::Bool { default } => (ParamType::Bool, Value::from(*default)),
        ParamKind::Int {
            default,
            min,
            max,
            step,
            unit,
        } => (
            ParamType::Int {
                min: *min,
                max: *max,
                step: *step,
                unit: unit.clone(),
            },
            Value::from(*default),
        ),
        ParamKind::Float {
            default,
            min,
            max,
            step,
            unit,
        } => (
            ParamType::Float {
                min: *min,
                max: *max,
                step: *step,
                unit: unit.clone(),
            },
            Value::from(*default),
        ),
        ParamKind::Enum { default, options } => (
            ParamType::Enum {
                options: options
                    .iter()
                    .map(|o| EnumOption {
                        value: o.value.clone(),
                        label: o.label.clone(),
                    })
                    .collect(),
            },
            Value::from(default.clone()),
        ),
        ParamKind::Color { default, alpha } => (
            ParamType::Color { alpha: *alpha },
            Value::from(default.clone()),
        ),
        ParamKind::Scene => (ParamType::Scene, Value::Null),
    };
    ParamDescriptor {
        id: param.id.clone(),
        ty,
        default,
        label: param.label.clone(),
        description: param.description.clone(),
        group: param.group.clone(),
        advanced: param.advanced,
        visible_if: None,
        live: true,
    }
}
