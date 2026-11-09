//! The `model` source (ADR-0107): a glTF 2.0 file or a built-in shape, drawn by the
//! engine in 3D where an item places it.
//!
//! The model is built when the source is created or a setting changes, off the render
//! thread: the file read through the asset roots, its images decoded by the image
//! source's decoders, its mesh checked against ADR-0107's caps. The source's frame is a
//! transparent stand-in at the model's size (its bounds' width and height, the larger
//! [`NATURAL_SIDE`] px), which is what `source.sizes` reports and a new item is sized by;
//! the picture itself is [`Source::model`].

mod gltf;

use std::sync::Arc;

use jstream_media::model::shapes::Shape;
use jstream_media::model::{Lighting, Material, Model};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{
    AssetRoot, EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings,
};
use serde_json::{Value, json};

use crate::color::{parse_color, rgba_frame};
use crate::describe::{asset, kind, param};
use crate::image::decode::decode;
use crate::source::{self, changed_non_live, setting};
use crate::{Context, ShownModel, Source, SourceError, SourceKind};

/// The longer side of a model's stand-in frame, in pixels: the size `source.sizes`
/// reports for it (ADR-0107 decision 8).
pub const NATURAL_SIDE: u32 = 512;

/// The `model` kind.
pub struct ModelKind(KindDescriptor);

impl ModelKind {
    /// The kind and its settings (ADR-0107 decision 1).
    pub fn new() -> Self {
        let options = |list: &[(&str, &str)]| ParamType::Enum {
            options: list
                .iter()
                .map(|(value, label)| EnumOption {
                    value: (*value).into(),
                    label: (*label).into(),
                })
                .collect(),
        };
        let unit = || ParamType::Float {
            min: Some(0.0),
            max: Some(1.0),
            step: Some(0.01),
            unit: None,
        };
        let described = |p: ParamDescriptor, text: &str| ParamDescriptor {
            description: Some(text.into()),
            ..p
        };
        let live = |p: ParamDescriptor| ParamDescriptor { live: true, ..p };
        Self(KindDescriptor {
            params: vec![
                described(
                    param(
                        "shape",
                        "Shape",
                        options(&[
                            ("file", "From a file"),
                            ("plane", "Plane"),
                            ("cube", "Cube"),
                            ("sphere", "Sphere"),
                            ("cylinder", "Cylinder"),
                            ("cone", "Cone"),
                        ]),
                        json!("file"),
                    ),
                    "A glTF file, or a built-in shape one unit across.",
                ),
                described(
                    param("file", "Model file", asset(&["glb", "gltf"]), Value::Null),
                    "A glTF 2.0 file (.glb, or .gltf with its files beside it), for the shape `file`.",
                ),
                live(described(
                    param(
                        "color",
                        "Color",
                        ParamType::Color { alpha: true },
                        json!("#ffffffff"),
                    ),
                    "A shape's base colour; multiplies a file's own.",
                )),
                live(described(
                    param("metallic", "Metallic", unit(), json!(0.0)),
                    "How metallic a shape is. A file's materials keep their own.",
                )),
                live(described(
                    param("roughness", "Roughness", unit(), json!(0.5)),
                    "How rough a shape is, 0 a mirror. A file's materials keep their own.",
                )),
                live(described(
                    param(
                        "lighting",
                        "Lighting",
                        options(&[("studio", "Studio"), ("unlit", "Unlit"), ("scene", "Scene")]),
                        json!("studio"),
                    ),
                    "Studio lights it from above and to the left of the camera; unlit shows its colours as they are; scene lights it with the light sources placed in its 3D scene (studio in a 2D scene).",
                )),
                live(described(
                    param(
                        "double_sided",
                        "Double-sided",
                        ParamType::Bool,
                        json!(false),
                    ),
                    "Draw a shape's back faces too. A file's materials keep their own.",
                )),
            ],
            ..kind("model", "3D model", true, false)
        })
    }
}

impl Default for ModelKind {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceKind for ModelKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    /// Reads and builds the model, so this blocks on I/O, decoding and checking.
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let base = build(settings, ctx)?;
        let look = Look::of(settings)?;
        let shown = base.as_ref().map(|base| look.apply(base));
        Ok(Box::new(ModelSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            frame: stand_in(base.as_deref())?,
            base,
            look,
            shown,
        }))
    }
}

/// The live settings that change a model's look, not its mesh.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    color: [f32; 4],
    metallic: f32,
    roughness: f32,
    lighting: Lighting,
    double_sided: bool,
}

impl Look {
    fn of(settings: &Settings) -> Result<Self, SourceError> {
        let bad = |param: &str, reason: &str| SourceError::Setting {
            param: param.into(),
            reason: reason.into(),
        };
        let rgba = setting(settings, "color")?
            .as_str()
            .and_then(parse_color)
            .ok_or_else(|| bad("color", "must be a color as `#rrggbb` or `#rrggbbaa`"))?;
        let unit = |id: &str| -> Result<f32, SourceError> {
            setting(settings, id)?
                .as_f64()
                .filter(|v| (0.0..=1.0).contains(v))
                .map(|v| v as f32)
                .ok_or_else(|| bad(id, "must be a number from 0 to 1"))
        };
        let lighting = setting(settings, "lighting")?
            .as_str()
            .and_then(Lighting::from_name)
            .ok_or_else(|| bad("lighting", "must be `studio`, `unlit` or `scene`"))?;
        let double_sided = setting(settings, "double_sided")?
            .as_bool()
            .ok_or_else(|| bad("double_sided", "must be true or false"))?;
        // The colour setting is sRGB; a material's base colour is linear.
        let decode = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        Ok(Self {
            color: [
                decode(rgba[0]),
                decode(rgba[1]),
                decode(rgba[2]),
                f32::from(rgba[3]) / 255.0,
            ],
            metallic: unit("metallic")?,
            roughness: unit("roughness")?,
            lighting,
            double_sided,
        })
    }

    /// The base model with this look: a shape takes every field, a file only the colour
    /// as a multiplier.
    fn apply(&self, base: &Base) -> ShownModel {
        let model = match base {
            Base::Shape(model) => model.with_materials(|m| {
                m.base_color = self.color;
                m.metallic = self.metallic;
                m.roughness = self.roughness;
                m.double_sided = self.double_sided;
            }),
            Base::File(model) => model.with_materials(|m| {
                for (c, tint) in m.base_color.iter_mut().zip(self.color) {
                    *c *= tint;
                }
            }),
        };
        ShownModel {
            model: Arc::new(model),
            lighting: self.lighting,
        }
    }
}

/// The model before the live settings: a shape, or what a file holds.
#[derive(Debug)]
enum Base {
    Shape(Model),
    File(Model),
}

impl std::ops::Deref for Base {
    type Target = Model;

    fn deref(&self) -> &Model {
        match self {
            Base::Shape(model) | Base::File(model) => model,
        }
    }
}

/// The model the `shape` and `file` settings name: `None` for the shape `file` with no
/// file chosen.
fn build(settings: &Settings, ctx: &Context) -> Result<Option<Base>, SourceError> {
    let shape = setting(settings, "shape")?
        .as_str()
        .ok_or_else(|| SourceError::Setting {
            param: "shape".into(),
            reason: "must be a string".into(),
        })?;
    if shape != "file" {
        let shape = Shape::from_name(shape).ok_or_else(|| SourceError::Setting {
            param: "shape".into(),
            reason: format!("`{shape}` is not a shape"),
        })?;
        let model = shape
            .model(Material::default())
            .map_err(|err| SourceError::Setting {
                param: "shape".into(),
                reason: err.to_string(),
            })?;
        return Ok(Some(Base::Shape(model)));
    }
    let Some((root, path)) = source::asset(settings, "file")? else {
        return Ok(None);
    };
    let bytes = ctx.assets.read(root, &path, ctx.limits.max_file_bytes)?;
    let mut files = Files {
        ctx,
        root,
        path: &path,
        error: None,
    };
    let loaded = gltf::load(&bytes, &mut files);
    // A failed read or decode keeps its own error, which names the file it was about.
    if let Some(err) = files.error {
        return Err(err);
    }
    let (model, ignored) = loaded.map_err(|err| SourceError::Model {
        path: path.clone(),
        reason: err.0,
    })?;
    for what in ignored.0 {
        tracing::info!(path = %path, "the model's {what} are not drawn");
    }
    Ok(Some(Base::File(model)))
}

/// The files a `.gltf` names, read beside it under the same asset root.
struct Files<'a> {
    ctx: &'a Context,
    root: AssetRoot,
    path: &'a str,
    /// The first read or decode that failed, kept whole.
    error: Option<SourceError>,
}

impl gltf::Resources for Files<'_> {
    fn read(&mut self, uri: &str) -> Result<Vec<u8>, gltf::LoadError> {
        let folder = match self.path.rfind(['/', '\\']) {
            Some(at) => &self.path[..=at],
            None => "",
        };
        let joined = format!("{folder}{uri}");
        self.ctx
            .assets
            .read(self.root, &joined, self.ctx.limits.max_file_bytes)
            .map_err(|err| {
                let message = err.to_string();
                self.error.get_or_insert(err);
                gltf::LoadError(message)
            })
    }

    fn decode(&mut self, bytes: &[u8], name: &str) -> Result<(u32, u32, Vec<u8>), gltf::LoadError> {
        let path = format!("{} ({name})", self.path);
        let decoded = decode(bytes, &path, &self.ctx.limits).map_err(|err| {
            let message = err.to_string();
            self.error.get_or_insert(err);
            gltf::LoadError(message)
        })?;
        let (width, height) = (decoded.width, decoded.height);
        let rgba = decoded
            .frames
            .into_iter()
            .next()
            .map(|(rgba, _)| rgba)
            .ok_or_else(|| gltf::LoadError(format!("{name} has no frame")))?;
        Ok((width, height, rgba))
    }
}

/// A transparent frame the size of the model's bounds, its larger side
/// [`NATURAL_SIDE`]; square for no model or a flat one.
fn stand_in(model: Option<&Model>) -> Result<Arc<VideoFrame>, SourceError> {
    let (w, h) = model.map_or((1.0, 1.0), |m| {
        let [w, h, _] = m.bounds().size();
        (w.max(0.0), h.max(0.0))
    });
    let side = |v: f32, of: f32| {
        if of <= 0.0 {
            NATURAL_SIDE
        } else {
            ((v / of) * NATURAL_SIDE as f32).round().max(1.0) as u32
        }
    };
    let longest = w.max(h);
    let size = FrameSize::new(side(w, longest).max(1), side(h, longest).max(1))?;
    let pixels = vec![0; (size.width() * size.height() * 4) as usize];
    Ok(Arc::new(rgba_frame(
        size,
        pixels,
        MediaTime::zero(Timebase::MILLIS),
    )?))
}

/// A running `model` source.
struct ModelSource {
    descriptor: KindDescriptor,
    settings: Settings,
    frame: Arc<VideoFrame>,
    base: Option<Base>,
    look: Look,
    shown: Option<ShownModel>,
}

impl Source for ModelSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let look = Look::of(settings)?;
        if look != self.look {
            self.shown = self.base.as_ref().map(|base| look.apply(base));
            self.look = look;
        }
        self.settings = settings.clone();
        Ok(())
    }

    fn model(&self) -> Option<ShownModel> {
        self.shown.clone()
    }
}

#[cfg(test)]
mod tests;
