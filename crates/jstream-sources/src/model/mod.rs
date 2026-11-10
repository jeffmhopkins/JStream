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
                live(described(
                    param(
                        "casts_shadows",
                        "Casts shadows",
                        ParamType::Bool,
                        json!(true),
                    ),
                    "Whether it blocks the light of its scene's lights that cast shadows.",
                )),
                live(described(
                    param(
                        "receives_shadows",
                        "Receives shadows",
                        ParamType::Bool,
                        json!(true),
                    ),
                    "Whether, lit by its scene, it is shadowed by the lights that cast them.",
                )),
                live(described(
                    param("animation", "Animation", ParamType::String { multiline: false, max_len: 256 }, json!("")),
                    "The file's animation to play, by its name; empty plays none and stands it at rest. A name the file lacks is refused with the names it has.",
                )),
                live(described(
                    param(
                        "animation_speed",
                        "Animation speed",
                        ParamType::Float {
                            min: Some(0.0),
                            max: Some(8.0),
                            step: Some(0.05),
                            unit: None,
                        },
                        json!(1.0),
                    ),
                    "How fast it plays: 1 at its own pace, 0 held at its start.",
                )),
                live(described(
                    param("animation_loop", "Loop the animation", ParamType::Bool, json!(true)),
                    "Whether it plays again from its start; if not, it holds its last frame.",
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
        let play = Play::of(settings, base.as_ref())?;
        let shown = base.as_ref().map(|base| look.apply(base));
        Ok(Box::new(ModelSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            frame: stand_in(base.as_deref())?,
            base,
            look,
            play,
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
    casts_shadows: bool,
    receives_shadows: bool,
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
        // Settings saved before shadows had neither: both on.
        let flag = |id: &str| match settings.get(id) {
            None => Ok(true),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| bad(id, "must be true or false")),
        };
        let (casts_shadows, receives_shadows) = (flag("casts_shadows")?, flag("receives_shadows")?);
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
            casts_shadows,
            receives_shadows,
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
            casts_shadows: self.casts_shadows,
            receives_shadows: self.receives_shadows,
            pose: None,
        }
    }
}

/// The live settings that play a file's animation (ADR-0151 decision 7).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Play {
    /// The clip, `None` for none (standing at rest).
    clip: Option<usize>,
    speed: f64,
    looped: bool,
}

impl Play {
    fn of(settings: &Settings, base: Option<&Base>) -> Result<Self, SourceError> {
        let bad = |param: &str, reason: String| SourceError::Setting {
            param: param.into(),
            reason,
        };
        // Settings saved before animation had none of these: play none.
        let name = match settings.get("animation") {
            None | Some(Value::Null) => "",
            Some(value) => value
                .as_str()
                .ok_or_else(|| bad("animation", "must be a string".into()))?,
        };
        let speed = match settings.get("animation_speed") {
            None => 1.0,
            Some(value) => value
                .as_f64()
                .filter(|v| (0.0..=8.0).contains(v))
                .ok_or_else(|| bad("animation_speed", "must be a number from 0 to 8".into()))?,
        };
        let looped = match settings.get("animation_loop") {
            None => true,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| bad("animation_loop", "must be true or false".into()))?,
        };
        let clip = match (name, base) {
            ("", _) | (_, None | Some(Base::Shape(_))) => None,
            (name, Some(Base::File(model))) => {
                let clips = model.rig().map(|r| r.clips()).unwrap_or_default();
                let found = clips.iter().position(|c| c.name == name);
                if found.is_none() {
                    let names: Vec<&str> = clips.iter().map(|c| c.name.as_str()).collect();
                    return Err(bad(
                        "animation",
                        if names.is_empty() {
                            format!("the file has no animation `{name}`: it has none")
                        } else {
                            format!(
                                "the file has no animation `{name}`; it has {}",
                                names.join(", ")
                            )
                        },
                    ));
                }
                found
            }
        };
        Ok(Self {
            clip,
            speed,
            looped,
        })
    }

    /// The time into the clip at `at` since the source started: `at` times the speed,
    /// looped over the clip's length or held at its end.
    fn seconds(&self, at: MediaTime, duration: f64) -> f64 {
        let t = at.as_secs_f64().max(0.0) * self.speed;
        if duration <= 0.0 {
            0.0
        } else if self.looped {
            t.rem_euclid(duration)
        } else {
            t.min(duration)
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
    play: Play,
    shown: Option<ShownModel>,
}

impl ModelSource {
    /// Poses the shown model for `at`, when it plays a clip.
    fn pose(&mut self, at: MediaTime) {
        let (Some(clip), Some(base), Some(shown)) = (self.play.clip, &self.base, &mut self.shown)
        else {
            return;
        };
        let Some(rig) = base.rig() else {
            return;
        };
        let duration = rig.clips().get(clip).map_or(0.0, |c| c.duration);
        let seconds = self.play.seconds(at, duration);
        shown.pose = Some(rig.pose(clip, seconds).into());
    }

    /// Whether the picture moves on from `at`: a clip playing, not yet held at its end.
    fn moving(&self, at: MediaTime) -> bool {
        let (Some(clip), Some(rig)) = (self.play.clip, self.base.as_ref().and_then(|b| b.rig()))
        else {
            return false;
        };
        let duration = rig.clips().get(clip).map_or(0.0, |c| c.duration);
        self.play.speed > 0.0
            && duration > 0.0
            && (self.play.looped || at.as_secs_f64() * self.play.speed < duration)
    }
}

impl Source for ModelSource {
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        self.pose(at);
        Ok(Arc::clone(&self.frame))
    }

    /// A playing clip moves on every tick; the stand-in frame itself never changes.
    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.moving(at).then(|| {
            at.checked_add(MediaTime::new(1, at.timebase()))
                .unwrap_or(at)
        })
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let look = Look::of(settings)?;
        let play = Play::of(settings, self.base.as_ref())?;
        if look != self.look {
            self.shown = self.base.as_ref().map(|base| look.apply(base));
            self.look = look;
        }
        if play != self.play {
            if let Some(shown) = &mut self.shown {
                shown.pose = None;
            }
            self.play = play;
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
