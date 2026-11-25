//! Drawing 3D models (ADR-0107 decisions 3, 4 and 6): a [`jstream_media::model::Model`]
//! uploaded once per version, then drawn through a scene camera (or an item's own
//! viewer) into a canvas-sized layer with its own depth test.
//!
//! The layer is premultiplied linear, like every layer, so filters run over it and a
//! scene places it with [`crate::Placement::Layer`], which sorts it with the scene's other
//! items at the model's centre.

use std::sync::{Arc, Mutex, PoisonError};

use jstream_media::model::{AlphaMode, Model, ModelTexture, TextureColor, TextureRef, rig};
use jstream_protocol::{Canvas, ItemGeometry, SceneCamera, Solid, SolidBounds};
use wgpu::util::DeviceExt;

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::image::{DEPTH_LAYER_FORMAT, Image, LAYER_FORMAT, Owner};
use crate::mips::{Mips, Region};
use crate::scene::Color;

use msaa::Drawn;

/// The model shader, after the filters' colour helpers, so a picture decodes as every
/// pass decodes it.
const SOURCE: &str = concat!(
    include_str!("../filter/colour.wgsl"),
    include_str!("model.wgsl")
);

/// The depth format: reversed, so `f32`'s precision near 0 is spent far away.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Bytes of one vertex: position, normal, tangent, texture coordinate and colour.
const VERTEX_STRIDE: u64 = jstream_media::model::VERTEX_BYTES;
/// The most point, spot and directional lights a `scene`-lit model is shaded with
/// (ADR-0143 decision 3); any after them are ignored.
pub const MAX_SCENE_LIGHTS: usize = 8;
/// Bytes of one light in the frame uniform: four `vec4`.
const LIGHT_BYTES: u64 = 64;
/// The most lights whose shadows a model receives (ADR-0151 decision 3): the first two
/// of its scene's spot and directional lights that cast them.
pub const MAX_SHADOW_MAPS: usize = 2;
/// A shadow map's width and height, in pixels (ADR-0151 decision 3).
pub const SHADOW_MAP_PX: u32 = 2048;
/// Where a material's bind group holds the sampler of its normal, surface, occlusion and
/// emissive maps, after its five textures.
const DETAIL_SAMPLER_BINDING: u32 = 7;
/// That sampler's anisotropy: the base colour's is 16.
const DETAIL_ANISOTROPY: u16 = 4;
/// Bytes of one shadow map's view in the frame uniform: four `vec4`.
const SHADOW_BYTES: u64 = 64;
/// Bytes of the environment in the frame uniform: six `vec4`.
const ENVIRONMENT_BYTES: u64 = 96;
/// Bytes of the frame uniform: two `mat4x4`, a `mat3x3` (three padded columns), a
/// `vec4` of parameters, a `vec4` of ambient light, the lights, the shadow maps' views,
/// the environment and the floor's cutting plane.
const FRAME_BYTES: u64 = 64
    + 64
    + 48
    + 16
    + 16
    + MAX_SCENE_LIGHTS as u64 * LIGHT_BYTES
    + MAX_SHADOW_MAPS as u64 * SHADOW_BYTES
    + ENVIRONMENT_BYTES
    + 16
    + PROBE_BYTES;
/// Bytes of the reflection probe in the frame uniform: five `vec4`.
const PROBE_BYTES: u64 = 80;
/// The frame's textures: the shadow maps, then the environment's picture, then the
/// reflection probe's (ADR-0170 decision 3).
const FRAME_TEXTURES: usize = MAX_SHADOW_MAPS + 2;
/// Where the sky uniform ([`environment_values`], [`reflection_values`]) holds how its
/// picture is encoded.
const SKY_MODE: usize = 11;
/// Where the probe uniform ([`probe_values`]) holds how its capture is encoded.
const PROBE_MODE: usize = 17;
/// The slot of [`FRAME_TEXTURES`] the reflection probe's picture is bound in.
const PROBE_SLOT: usize = MAX_SHADOW_MAPS + 1;

/// The mip chains a draw's blurred reflections read (ADR-0170 decision 3): of the sky
/// picture it reflects and of its reflection probe's capture, each with the image it
/// was built from. A rough surface reads the level whose texels are about as wide as
/// its blur's taps are apart, so a bright point spreads into a soft patch rather than
/// one sharp copy per tap. A draw without a chain reads the picture itself, unblurred
/// between its taps.
#[derive(Clone, Copy, Default)]
pub(crate) struct Panoramas<'a> {
    /// The sky picture's chain: the model's `reflection`, else its environment's
    /// picture.
    pub sky: Option<(&'a Image, &'a Mips)>,
    /// The probe's capture's chain.
    pub probe: Option<(&'a Image, &'a Mips)>,
}

impl<'a> Panoramas<'a> {
    /// The blurred levels of `chain` (`panorama_blur.rs`) when it was built from
    /// `image`.
    fn of(
        chain: Option<(&'a Image, &'a Mips)>,
        image: &Image,
    ) -> Option<&'a (wgpu::Texture, wgpu::TextureView)> {
        chain
            .filter(|(built, _)| built.texture() == image.texture())
            .and_then(|(_, mips)| mips.blurred.get())
            .map(|blurred| &blurred.whole)
    }
}
/// Bytes of a material's uniform: five `vec4`.
const MATERIAL_BYTES: u64 = 80;
/// The most bytes of a texture level one queue write copies ([`upload_texture`]).
const TEXTURE_BAND_BYTES: usize = 4 << 20;

pub use jstream_media::model::Lighting;

/// How many sizes of the depth buffer a model's layer is drawn with are kept
/// (`ModelRuntime::depth_view`).
const KEPT_DEPTHS: usize = 2;

/// A model on the GPU, made by [`crate::Compositor::upload_model`]. Cheap to clone; a
/// clone shares the buffers and textures.
#[derive(Clone)]
pub struct GpuModel {
    inner: Arc<Uploaded>,
}

struct Uploaded {
    owner: Owner,
    version: u64,
    bounds: SolidBounds,
    primitives: Vec<GpuPrimitive>,
    materials: Vec<GpuMaterial>,
    /// A rigged model's palette (ADR-0151 decision 7): its buffer, which
    /// [`crate::Compositor::pose_model`] writes, its bind group and its slot count.
    pose: Option<(wgpu::Buffer, wgpu::BindGroup, usize)>,
    /// The material bind groups of its last draw with a live texture (ADR-0107
    /// decision 5), kept while that texture and the material it replaces stay the same,
    /// so a steady frame makes none.
    live: Mutex<Option<LiveBinds>>,
    /// Each texture's format as uploaded, for tests.
    #[cfg(test)]
    formats: Vec<wgpu::TextureFormat>,
}

/// Material bind groups with a live texture in place of their base colour's.
struct LiveBinds {
    /// The texture they bind: a source's mip chain, or the transparent stand-in.
    texture: wgpu::Texture,
    /// The material they replace it in, or every material.
    material: Option<String>,
    /// One per material: `Some` for a material whose texture is replaced, with the
    /// uniform it binds, which a draw that drives the material writes (ADR-0171).
    binds: Vec<Option<(wgpu::BindGroup, wgpu::Buffer)>>,
}

struct GpuPrimitive {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    material: usize,
    /// The middle of its vertices' bounds, which a blended primitive sorts by.
    center: [f32; 3],
}

struct GpuMaterial {
    bind: wgpu::BindGroup,
    blended: bool,
    /// Its name in the file, which a [`MaterialDrive`] and a live texture name it by.
    name: String,
    /// Its uniform as uploaded, which a draw that drives it changes and a later one
    /// that doesn't puts back.
    values: [f32; MATERIAL_VALUES],
    uniform: wgpu::Buffer,
    /// Whether its uniform holds a drive's values instead of `values`.
    driven: std::sync::atomic::AtomicBool,
    /// Whether its live texture's uniform does.
    live_driven: std::sync::atomic::AtomicBool,
    /// Its textures' views, to bind it again with a live texture (ADR-0107 decision 5).
    views: [wgpu::TextureView; 5],
}

/// How many `f32`s a material's uniform holds.
const MATERIAL_VALUES: usize = 20;

/// How one placement of a model changes some of its materials as it draws them, by
/// motion modifiers on its source's settings (ADR-0171 decision 4). Nothing is stored:
/// the next draw that doesn't drive a material draws it as the file has it. Each value
/// is added: colours as a fraction of full to each of their sRGB red, green and blue,
/// kept within 0 and full, the rest kept within their ranges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaterialDrive<'a> {
    /// The materials it changes, by their name in the file; `None` for every material.
    pub material: Option<&'a str>,
    /// Added to the base colour (its alpha stays).
    pub color: f32,
    /// Added to metalness, kept within 0 and 1.
    pub metallic: f32,
    /// Added to roughness, kept within 0 and 1.
    pub roughness: f32,
    /// Added to the glow's colour, at its own brightness.
    pub emissive: f32,
    /// Added to the glow's strength, a multiplier that is 1 at rest, kept within 0 and
    /// [`MAX_EMISSIVE_STRENGTH`].
    pub emissive_strength: f32,
    /// Replaces the base colour, straight sRGB 0 to 1, before `color` adds to it (its
    /// alpha stays). The last drive that sets one wins.
    pub set_color: Option<[f32; 3]>,
    /// Replaces the glow's colour, straight sRGB 0 to 1, at its own brightness, before
    /// `emissive` adds to it.
    pub set_emissive: Option<[f32; 3]>,
}

/// The most a [`MaterialDrive`] multiplies a material's glow by.
pub const MAX_EMISSIVE_STRENGTH: f32 = 64.0;

impl MaterialDrive<'_> {
    /// Whether it changes `name`.
    fn reaches(&self, name: &str) -> bool {
        self.material.is_none_or(|m| m == name)
    }
}

/// `values`, a material's uniform, with `drives`' sums applied, or `None` when they
/// add nothing to it.
fn driven_values(
    values: &[f32; MATERIAL_VALUES],
    name: &str,
    drives: &[MaterialDrive<'_>],
) -> Option<[f32; MATERIAL_VALUES]> {
    let mut add = MaterialDrive::default();
    for d in drives.iter().filter(|d| d.reaches(name)) {
        add.color += d.color;
        add.metallic += d.metallic;
        add.roughness += d.roughness;
        add.emissive += d.emissive;
        add.emissive_strength += d.emissive_strength;
        add.set_color = d.set_color.or(add.set_color);
        add.set_emissive = d.set_emissive.or(add.set_emissive);
    }
    if add == MaterialDrive::default() {
        return None;
    }
    let mut out = *values;
    // Base colour, linear: set, then lightened in sRGB, as a colour setting is.
    if let Some(color) = add.set_color {
        for (v, c) in out[0..3].iter_mut().zip(color) {
            *v = srgb_to_linear(c.clamp(0.0, 1.0));
        }
    }
    for v in &mut out[0..3] {
        *v = srgb_to_linear((linear_to_srgb(*v) + add.color).clamp(0.0, 1.0));
    }
    // Emission may exceed 1: set and lightened at its own brightness, then scaled.
    let peak = out[4..7].iter().copied().fold(0.0f32, f32::max).max(1.0);
    if let Some(color) = add.set_emissive {
        for (v, c) in out[4..7].iter_mut().zip(color) {
            *v = srgb_to_linear(c.clamp(0.0, 1.0)) * peak;
        }
    }
    for v in &mut out[4..7] {
        let lifted = srgb_to_linear((linear_to_srgb(*v / peak) + add.emissive).clamp(0.0, 1.0));
        *v = lifted * peak * (1.0 + add.emissive_strength).clamp(0.0, MAX_EMISSIVE_STRENGTH);
    }
    out[8] = (out[8] + add.metallic).clamp(0.0, 1.0);
    out[9] = (out[9] + add.roughness).clamp(0.0, 1.0);
    out.iter().all(|v| v.is_finite()).then_some(out)
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// `values` as the bytes of a uniform, on the stack.
fn material_bytes(values: &[f32; MATERIAL_VALUES]) -> [u8; MATERIAL_VALUES * 4] {
    let mut out = [0u8; MATERIAL_VALUES * 4];
    for (chunk, v) in out.chunks_exact_mut(4).zip(values) {
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// The texels of a `size` target that a model with `bounds` can cover when drawn through
/// `clip` (a model-to-clip matrix, column-major), with a texel of margin, clipped: the
/// box of its bounds' eight corners. `None` when a corner is at or behind the viewer,
/// where the box has no end, or isn't finite. Empty for a model off the target.
fn drawn_box(clip: &[[f32; 4]; 4], bounds: SolidBounds, size: (u32, u32)) -> Option<Region> {
    let (width, height) = (f64::from(size.0), f64::from(size.1));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for corner in 0..8 {
        let pick = |bit: usize, axis: usize| {
            f64::from(if corner & (1 << bit) == 0 {
                bounds.min[axis]
            } else {
                bounds.max[axis]
            })
        };
        let at = [pick(0, 0), pick(1, 1), pick(2, 2), 1.0];
        let row = |r: usize| (0..4).map(|c| f64::from(clip[c][r]) * at[c]).sum::<f64>();
        let (x, y, w) = (row(0), row(1), row(3));
        if !(w > 1e-6 && x.is_finite() && y.is_finite()) {
            return None;
        }
        let (px, py) = ((x / w * 0.5 + 0.5) * width, (0.5 - y / w * 0.5) * height);
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    let low = |v: f64, side: f64| (v.floor() - 1.0).clamp(0.0, side) as u32;
    let high = |v: f64, side: f64| (v.ceil() + 1.0).clamp(0.0, side) as u32;
    let (x0, y0) = (low(x0, width), low(y0, height));
    let (x1, y1) = (high(x1, width), high(y1, height));
    Some(Region {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    })
}

/// The smallest region holding `a` and `b`; an empty one adds nothing.
fn union(a: Region, b: Region) -> Region {
    if a.width == 0 || a.height == 0 {
        return b;
    }
    if b.width == 0 || b.height == 0 {
        return a;
    }
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Region {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

/// Which uploaded model a [`GpuModel`] was ([`GpuModel::seen`]). It doesn't keep the
/// model's buffers alive, and it never matches another upload, even one made where a
/// dropped one was.
#[derive(Clone, Debug)]
pub struct SeenModel(std::sync::Weak<Uploaded>);

impl PartialEq for SeenModel {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Weak::ptr_eq(&self.0, &other.0)
    }
}

impl GpuModel {
    /// Each texture's format as uploaded.
    #[cfg(test)]
    pub(crate) fn texture_formats(&self) -> &[wgpu::TextureFormat] {
        &self.inner.formats
    }

    /// Which upload this is, to tell later whether a model is still the same one.
    pub fn seen(&self) -> SeenModel {
        SeenModel(Arc::downgrade(&self.inner))
    }

    /// The version of the model it was uploaded from
    /// ([`jstream_media::model::Model::version`]).
    pub fn version(&self) -> u64 {
        self.inner.version
    }

    /// The model's bounds, in its own space.
    pub fn bounds(&self) -> SolidBounds {
        self.inner.bounds
    }
}

impl std::fmt::Debug for GpuModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuModel")
            .field("version", &self.inner.version)
            .field("primitives", &self.inner.primitives.len())
            .finish_non_exhaustive()
    }
}

/// One placement of a model: what [`crate::Compositor::draw_model`] draws.
#[derive(Clone, Copy, Debug)]
pub struct ModelDraw<'a> {
    /// The model. It must have been uploaded by the compositor that draws it.
    pub model: &'a GpuModel,
    /// The item's geometry: the model's pose (ADR-0107 decision 3).
    pub geometry: ItemGeometry,
    /// The camera of the 3D scene the item is in, or `None` in a 2D scene.
    pub camera: Option<SceneCamera>,
    /// How it is lit.
    pub lighting: Lighting,
    /// The lights of the scene the item is in, which light the model in place of the
    /// studio rig (ADR-0143 decision 3), or `None` for `lighting`'s own. A caller passes
    /// `Some` only for a model whose source asks to be lit by its scene, in the scene's
    /// stacking order before any depth sort; the first [`MAX_SCENE_LIGHTS`] that aren't
    /// ambient shade it, lit or not, and the caller logs any it drops. Only a 3D scene
    /// has them: without a `camera` they are ignored and `lighting` applies. An `unlit`
    /// model, and an unlit material, ignore them too.
    pub lights: Option<&'a [SceneLight]>,
    /// The shadow maps of `lights` that cast shadows (ADR-0151 decision 3), at most
    /// [`MAX_SHADOW_MAPS`]: each light's visibility at a fragment scales its light. Only
    /// a model lit by its scene uses them; empty for one that receives no shadows.
    pub shadows: &'a [ShadowMap<'a>],
    /// The scene's environment light (ADR-0151 decision 9): the sky a model lit by its
    /// scene takes its ambient light and reflections from, in place of the studio
    /// gradient. Ignored without `lights`.
    pub environment: Option<Environment<'a>>,
    /// A floor the model is cut at (ADR-0151 decision 4): nothing of it at or below the
    /// plane `y = floor` of the scene's space (y down) is drawn, or written to its view
    /// depth, so a scene drawn mirrored in its floor shows only what stands above it.
    /// `None` draws all of it; ignored without a `camera`.
    pub floor: Option<f32>,
    /// How this placement changes the model's materials as it draws them (ADR-0171
    /// decision 4); empty draws them as the file has them.
    pub materials: &'a [MaterialDrive<'a>],
    /// Another source's picture in place of the base-colour texture (ADR-0107
    /// decision 5), sampled through a mip chain the compositor builds from it once per
    /// change. `None` keeps the model's own textures.
    pub texture: Option<LiveTexture<'a>>,
    /// Another source's picture the model reflects (ADR-0107 decision 5), read as an
    /// equirectangular panorama as a scene's sky is: it replaces the studio
    /// environment, or for a model lit by its scene that scene's sky, for its
    /// reflections and ambient light. An unlit model ignores it.
    pub reflection: Option<&'a Image>,
    /// The reflection probe the model reads (ADR-0170 decision 3): inside its box the
    /// model reflects the probe's capture in place of the environment. Only a model lit
    /// by its scene reads one; `None` for none.
    pub probe: Option<ProbeLookup<'a>>,
}

/// A reflection probe's capture, as a surface reads it (ADR-0170 decision 3).
#[derive(Clone, Copy, Debug)]
pub struct ProbeLookup<'a> {
    /// The capture: an equirectangular panorama of the scene around `centre`, in the
    /// scene's axes, its centre along -z and up at its top, premultiplied with alpha
    /// where something was seen, as [`crate::Compositor::resample_probe`] draws it.
    pub image: &'a Image,
    /// The box's centre, in the scene's space, where the capture was taken.
    pub centre: [f32; 3],
    /// Half the box's size along the scene's x, y and z axes, in scene pixels.
    pub half: [f32; 3],
    /// How far inside its faces a surface fades from the environment to the probe, in
    /// scene pixels.
    pub fade: f32,
}

/// A source's picture on a model's surfaces (ADR-0107 decision 5).
#[derive(Clone, Copy, Debug)]
pub struct LiveTexture<'a> {
    /// The picture, made by the compositor that draws the model. `None` for a source
    /// that is missing or not drawing yet: the texture then reads transparent.
    pub image: Option<&'a Image>,
    /// The material, by its name in the file, whose base-colour texture it replaces;
    /// `None` replaces every material's.
    pub material: Option<&'a str>,
}

/// The bounds of the quad a lit card is shaded on: a unit square, flat.
const CARD_BOUNDS: SolidBounds = SolidBounds {
    min: [-0.5, -0.5, 0.0],
    max: [0.5, 0.5, 0.0],
};

/// A card lit by its 3D scene (ADR-0151 decision 10): what
/// [`crate::Compositor::light_card`] shades.
#[derive(Clone, Copy, Debug)]
pub struct CardDraw<'a> {
    /// The card's picture, flat in a layer: premultiplied linear light, as every layer
    /// is. It must have been made by the compositor that lights it.
    pub image: &'a Image,
    /// Where that layer sits in the scene: its box is the whole layer. Its fit mode and
    /// flips are ignored, since the layer already holds the picture as it is placed.
    pub geometry: ItemGeometry,
    /// The scene's camera, which the card's shading is seen from.
    pub camera: SceneCamera,
    /// As a model material's roughness, 0 to 1.
    pub roughness: f32,
    /// As a model material's metalness, 0 to 1.
    pub metalness: f32,
    /// How much of its own picture it adds back unlit, 0 to 4.
    pub emissive: f32,
    /// The scene's lights, as a `scene`-lit model receives them.
    pub lights: &'a [SceneLight],
    /// The shadow maps of `lights` it receives; empty for none.
    pub shadows: &'a [ShadowMap<'a>],
    /// The scene's environment light.
    pub environment: Option<Environment<'a>>,
    /// The reflection probe it reads (ADR-0170 decision 3), as a model does.
    pub probe: Option<ProbeLookup<'a>>,
}

/// The sky around a 3D scene (ADR-0151 decision 9), from its first environment light.
#[derive(Clone, Copy, Debug)]
pub struct Environment<'a> {
    /// The sky straight up, straight sRGB. Alpha is ignored.
    pub sky: Color,
    /// The sky at the horizon.
    pub horizon: Color,
    /// Below the horizon.
    pub ground: Color,
    /// Scales it: the light's intensity times its item's opacity.
    pub intensity: f32,
    /// How far it is spun about the vertical axis, in degrees: its item's turn.
    pub turn_deg: f32,
    /// How far it is pitched about the scene's x axis, in degrees: its item's tilt
    /// (ADR-0182).
    pub tilt_deg: f32,
    /// How far it is rolled about the scene's z axis, in degrees: its item's rotation
    /// (ADR-0182).
    pub roll_deg: f32,
    /// A picture that is the sky instead, as an equirectangular panorama: its centre
    /// straight ahead along -z, up at its top.
    pub image: Option<&'a Image>,
}

impl Environment<'_> {
    /// The camera's axes `[right, down, forward]` in the sky's own frame: each scene
    /// direction taken back through the sky's orientation, as an item's (ADR-0022's
    /// order: roll about z, then turn about y, then tilt about x, ADR-0166). The shaders
    /// read a direction's sky along these axes with no turn of their own, so the sky is
    /// oriented by all three of its item's angles (ADR-0182) and a turn alone spins it
    /// as before (ADR-0167).
    #[must_use]
    pub fn sky_axes(&self, axes: [[f64; 3]; 3]) -> [[f32; 3]; 3] {
        let rad = |deg: f32| {
            if deg.is_finite() {
                f64::from(deg.clamp(-1.0e6, 1.0e6)).to_radians()
            } else {
                0.0
            }
        };
        let (tilt, turn, roll) = (rad(self.tilt_deg), rad(self.turn_deg), rad(self.roll_deg));
        axes.map(|[x, y, z]| {
            // The inverse of the item's rotation: undo the tilt, then the turn, then
            // the roll.
            let (s, c) = (-tilt).sin_cos();
            let (y, z) = (y * c - z * s, y * s + z * c);
            let (s, c) = (-turn).sin_cos();
            let (x, z) = (x * c + z * s, -x * s + z * c);
            let (s, c) = (-roll).sin_cos();
            let (x, y) = (x * c - y * s, x * s + y * c);
            [x as f32, y as f32, z as f32]
        })
    }
}

/// One light's shadow map (ADR-0151 decision 3): the scene's casters' view depth drawn
/// from the light, as [`crate::Compositor::render_view_depth`] and
/// [`crate::Compositor::draw_model_depth`] draw it through `camera` into a square
/// layer.
#[derive(Clone, Copy, Debug)]
pub struct ShadowMap<'a> {
    /// Which of [`ModelDraw::lights`] casts it.
    pub light: usize,
    /// The map: red holds `1 / w` of the nearest caster seen from the light.
    pub image: &'a Image,
    /// The light's view: a perspective view along its aim, seen on the map's canvas.
    pub camera: SceneCamera,
    /// How soft the shadow's edge is, 0 (hard) to 1.
    pub softness: f32,
}

/// What kind of light a [`SceneLight`] is (ADR-0143 decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    /// From its position in every direction, fading out to its range.
    Point,
    /// From its position along its aim, within its cone, fading out to its range.
    Spot,
    /// Along its aim from infinitely far, with no falloff.
    Directional,
    /// From everywhere: scales the model's environment for its ambient and reflected
    /// light. Ambient lights add up.
    Ambient,
}

/// One light of a scene, as its item places it (ADR-0143 decisions 1 and 2), in the
/// scene's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneLight {
    /// What kind it is.
    pub kind: LightKind,
    /// Where it is, in the scene's space (x right, y down, z toward the viewer), for a
    /// point or spot light: its item's centre and `depth_px`.
    pub position: [f64; 3],
    /// The direction it shines, in the scene's space, for a spot or directional light:
    /// its item's back normal. Need not be unit length; a zero aim lights nothing.
    pub aim: [f64; 3],
    /// Its colour, straight sRGB. Alpha is ignored.
    pub color: Color,
    /// Irradiance on a surface facing it, before falloff: the kind's `intensity` times its
    /// item's opacity. At or under 0 it lights nothing but still takes its slot. A light
    /// with a non-finite intensity, range, cone, softness or (point or spot) position
    /// lights nothing too.
    pub intensity: f32,
    /// Where a point or spot light has faded to nothing, in scene pixels; 0 never fades.
    pub range_px: f32,
    /// A spot light's full cone angle, in degrees.
    pub cone_deg: f32,
    /// The share of a spot light's cone, from its edge inward, over which it fades, 0 to 1.
    pub softness: f32,
}

/// The pipelines, layouts and shared resources the model draw needs, and the depth
/// target it reuses.
pub(crate) struct ModelRuntime {
    material_layout: wgpu::BindGroupLayout,
    frame_layout: wgpu::BindGroupLayout,
    /// A rig's palette (ADR-0151 decision 7), read by the vertex shader.
    pose_layout: wgpu::BindGroupLayout,
    /// The palette of a model without a rig: one identity, which no vertex weighs.
    rest_pose: wgpu::BindGroup,
    /// The frame bind group with the shadow maps and environment picture of the last
    /// draw that had any, and their textures, so a steady frame makes no bind group
    /// (ADR-0151 decisions 3 and 9).
    shadow_bind: Option<([Option<wgpu::Texture>; FRAME_TEXTURES], wgpu::BindGroup)>,
    /// The layer's pipelines at each sample count drawn with (ADR-0242), made the
    /// first time a count is: single-sampled first.
    layer: Vec<(u32, LayerPipelines)>,
    /// The model shader and its pipeline layout, which a count's pipelines are made of.
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    /// The samples a model's layer is drawn with (ADR-0242), as
    /// [`Self::set_samples`] last set it and the device takes: 1 draws it aliased.
    samples: u32,
    /// The multisampled targets of the sizes drawn last, and the pass that resolves a
    /// multisampled surface buffer.
    msaa: msaa::Targets,
    surface_resolve: msaa::SurfaceResolve,
    /// The model's view depth (ADR-0143 decision 4), keeping the larger value.
    depth_only: wgpu::RenderPipeline,
    /// The view depth read from a layer's own tested depth instead.
    depth_resolve: depth_resolve::DepthResolve,
    /// What a screen-space reflection reads of each model (ADR-0170 decision 4), for a
    /// model whose layer was drawn without it.
    surface: wgpu::RenderPipeline,
    /// The layer's pipelines with a surface buffer beside it, and the merge of such a
    /// surface into its scene's buffer.
    with_surface: WithSurface,
    /// Its depth, kept across the models drawn into one set of targets.
    surface_depth: Option<(u32, u32, wgpu::TextureView)>,
    sampler: wgpu::Sampler,
    /// The sampler of a material's normal, surface, occlusion and emissive maps.
    detail_sampler: wgpu::Sampler,
    frame: wgpu::Buffer,
    frame_bind: wgpu::BindGroup,
    /// 1x1 stand-ins for a texture a material doesn't have: white for colour, occlusion
    /// and metal-roughness, and a flat normal.
    white: wgpu::TextureView,
    flat_normal: wgpu::TextureView,
    /// A live texture whose source isn't drawing: transparent, and its texture, which
    /// [`LiveBinds`] tells apart from a mip chain's.
    clear: (wgpu::Texture, wgpu::TextureView),
    depth: Vec<(u32, u32, wgpu::TextureView)>,
    /// Blended primitives in the order they draw, kept to reuse the allocation.
    order: Vec<(f64, usize)>,
    /// The quad a lit card is shaded on ([`Self::light_card`]), once made.
    card: Option<(wgpu::Buffer, wgpu::Buffer)>,
}

impl ModelRuntime {
    pub(crate) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        // The frame's uniform, the shadow maps (ADR-0151 decision 3), then the
        // environment's picture and its repeating sampler (decision 9).
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-model-frame"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(FRAME_BYTES),
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(5),
            ],
        });
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-model-material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(MATERIAL_BYTES),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                texture_entry(5),
                texture_entry(6),
                wgpu::BindGroupLayoutEntry {
                    binding: DETAIL_SAMPLER_BINDING,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jstream-render-model-pose"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let rest_pose = pose_bind(&gpu.device, &pose_layout, &[IDENTITY_MAT4]).1;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jstream-render-model"),
            source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jstream-render-model"),
            bind_group_layouts: &[
                Some(&frame_layout),
                Some(&material_layout),
                Some(&pose_layout),
            ],
            immediate_size: 0,
        });
        let first = layer_pipelines(gpu, (&shader, &layout), 1);
        let with_surface = WithSurface {
            merge: merge_pipeline(gpu),
        };
        let largest = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let depth_only = wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-model-view-depth"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x4,
                        3 => Float32x2,
                        4 => Float32x4,
                        5 => Uint16x4,
                        6 => Float32x4,
                    ],
                })],
            },
            // Both faces: the nearest of them wins the blend.
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_depth"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: DEPTH_LAYER_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: largest,
                        alpha: largest,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        };
        let depth_only = gpu.built.render(device, SOURCE, &(), &depth_only);
        // What a screen-space reflection reads (ADR-0170 decision 4): three targets, and
        // a depth test of its own so the nearest surface of every model drawn wins.
        let plain = Some(wgpu::ColorTargetState {
            format: LAYER_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        });
        let surface = wgpu::RenderPipelineDescriptor {
            label: Some("jstream-render-model-surface"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x4,
                        3 => Float32x2,
                        4 => Float32x4,
                        5 => Uint16x4,
                        6 => Float32x4,
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_surface"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[plain.clone(), plain.clone(), plain],
            }),
            multiview_mask: None,
            cache: None,
        };
        let surface = gpu.built.render(device, SOURCE, &(), &surface);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-model-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        // The maps that shape the light, not the picture: a quarter of the taps across a
        // grazing surface, where their detail is lost in the shading anyway.
        let detail_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("jstream-render-model-detail-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: DETAIL_ANISOTROPY,
            ..Default::default()
        });
        let frame = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render-model-frame"),
            size: FRAME_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let pixel = |label: &str, rgba: [u8; 4]| {
            let texture = device.create_texture_with_data(
                &gpu.queue,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba,
            );
            (
                texture.create_view(&wgpu::TextureViewDescriptor::default()),
                texture,
            )
        };
        let white = pixel("jstream-render-model-white", [255; 4]).0;
        let flat_normal = pixel("jstream-render-model-normal", [128, 128, 255, 255]).0;
        let (clear_view, clear_texture) = pixel("jstream-render-model-clear", [0; 4]);
        let frame_bind = frame_bind_group(
            device,
            &frame_layout,
            (&frame, &sampler),
            [&white, &white, &white, &white],
        );
        Self {
            material_layout,
            frame_layout,
            pose_layout,
            rest_pose,
            shadow_bind: None,
            layer: vec![(1, first)],
            shader,
            layout,
            samples: 1,
            msaa: msaa::Targets::default(),
            surface_resolve: msaa::SurfaceResolve::new(gpu),
            depth_only,
            depth_resolve: depth_resolve::DepthResolve::new(gpu),
            surface,
            with_surface,
            surface_depth: None,
            sampler,
            detail_sampler,
            frame,
            frame_bind,
            white,
            flat_normal,
            clear: (clear_texture, clear_view),
            depth: Vec::with_capacity(KEPT_DEPTHS),
            order: Vec::new(),
            card: None,
        }
    }

    /// What an upload of a model `owner`'s compositor draws takes of it.
    pub(crate) fn upload_kit(&self, gpu: &Gpu, owner: &Owner) -> UploadKit {
        UploadKit {
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
            max_texture_dimension_2d: gpu.max_texture_dimension_2d,
            max_buffer_size: gpu.max_buffer_size,
            owner: owner.clone(),
            material_layout: self.material_layout.clone(),
            pose_layout: self.pose_layout.clone(),
            sampler: self.sampler.clone(),
            detail_sampler: self.detail_sampler.clone(),
            white: self.white.clone(),
            flat_normal: self.flat_normal.clone(),
            compress: gpu
                .device
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
        }
    }
}

/// What uploading a model takes of the compositor that draws it: its device's handles
/// and the model pipelines' layouts, sampler and stand-in textures. It holds clones, so
/// an upload can run on another thread than the compositor's
/// ([`crate::ModelUploader`]). It writes buffers and textures and submits nothing, so
/// what it wrote goes with the queue's next submit, before any draw of the model.
pub(crate) struct UploadKit {
    device: wgpu::Device,
    queue: wgpu::Queue,
    max_texture_dimension_2d: u32,
    max_buffer_size: u64,
    owner: Owner,
    material_layout: wgpu::BindGroupLayout,
    pose_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    detail_sampler: wgpu::Sampler,
    white: wgpu::TextureView,
    flat_normal: wgpu::TextureView,
    /// Whether the device samples BC textures, so a model's textures are block
    /// compressed on upload (`bc.rs`).
    compress: bool,
}

impl UploadKit {
    /// The device it uploads to.
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Uploads `model`: its vertex and index buffers, its textures with their mips, and a
    /// bind group per material.
    pub(crate) fn upload(&self, model: &Model) -> Result<GpuModel, RenderError> {
        let device = &self.device;
        // Every primitive is one vertex and one index buffer, which wgpu panics on, rather
        // than refuses, past the device's limit: check before uploading anything.
        for p in model.primitives() {
            let vertex_bytes = p.vertices.len() as u64 * VERTEX_STRIDE;
            let index_bytes = p.indices.len() as u64 * 4;
            if vertex_bytes.max(index_bytes) > self.max_buffer_size {
                return Err(RenderError::Invalid {
                    what: "model",
                    detail: format!(
                        "a primitive of {} vertices and {} indices needs a {} byte buffer; this GPU takes {}",
                        p.vertices.len(),
                        p.indices.len(),
                        vertex_bytes.max(index_bytes),
                        self.max_buffer_size
                    ),
                });
            }
        }
        // A normal map stays as it is: BC1's four colours a block would bend its normals
        // into steps the light shows.
        let normals: std::collections::BTreeSet<usize> = model
            .materials()
            .iter()
            .filter_map(|m| m.normal_texture.map(|t| t.texture))
            .collect();
        let mut textures = Vec::with_capacity(model.textures().len());
        #[cfg(test)]
        let mut formats = Vec::with_capacity(model.textures().len());
        for (index, texture) in model.textures().iter().enumerate() {
            let compress = self.compress && !normals.contains(&index);
            let (view, _format) = upload_texture(self, texture, compress)?;
            #[cfg(test)]
            formats.push(_format);
            textures.push(view);
        }
        let view =
            |texture: Option<TextureRef>, fallback: &wgpu::TextureView| -> wgpu::TextureView {
                texture
                    .and_then(|t| textures.get(t.texture))
                    .unwrap_or(fallback)
                    .clone()
            };
        let mut materials = Vec::with_capacity(model.materials().len());
        for m in model.materials() {
            let (mode, cutoff) = match m.alpha_mode {
                AlphaMode::Opaque => (0.0, 0.0),
                AlphaMode::Mask { cutoff } => (1.0, cutoff),
                AlphaMode::Blend => (2.0, 0.0),
            };
            let values: [f32; MATERIAL_VALUES] = [
                m.base_color[0],
                m.base_color[1],
                m.base_color[2],
                m.base_color[3],
                m.emissive[0],
                m.emissive[1],
                m.emissive[2],
                if m.normal_texture.is_some() { 1.0 } else { 0.0 },
                m.metallic,
                m.roughness,
                m.normal_scale,
                m.occlusion_strength,
                mode,
                cutoff,
                if m.double_sided { 1.0 } else { 0.0 },
                if m.unlit { 1.0 } else { 0.0 },
                // A model's textures hold straight colour, not a card's picture.
                0.0,
                0.0,
                0.0,
                0.0,
            ];
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-model-material"),
                contents: &f32_bytes(&values),
                // Written again when a draw drives it (ADR-0171 decision 4).
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let views = [
                view(m.base_color_texture, &self.white),
                view(m.metallic_roughness_texture, &self.white),
                view(m.normal_texture, &self.flat_normal),
                view(m.occlusion_texture, &self.white),
                view(m.emissive_texture, &self.white),
            ];
            let mut entries = vec![
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: DETAIL_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(&self.detail_sampler),
                },
            ];
            for (i, view) in views.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: 2 + i as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("jstream-render-model-material"),
                layout: &self.material_layout,
                entries: &entries,
            });
            materials.push(GpuMaterial {
                bind,
                blended: matches!(m.alpha_mode, AlphaMode::Blend),
                name: m.name.clone(),
                values,
                uniform,
                driven: std::sync::atomic::AtomicBool::new(false),
                live_driven: std::sync::atomic::AtomicBool::new(false),
                views,
            });
        }
        let rest = model.rig().map(|r| r.rest()).unwrap_or_default();
        let mut primitives = Vec::with_capacity(model.primitives().len());
        for p in model.primitives() {
            if p.indices.is_empty() {
                continue;
            }
            let mut bytes = Vec::with_capacity(p.vertices.len() * VERTEX_STRIDE as usize);
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for v in &p.vertices {
                for value in v
                    .position
                    .iter()
                    .chain(&v.normal)
                    .chain(&v.tangent)
                    .chain(&v.uv)
                    .chain(&v.color)
                {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                for joint in v.joints {
                    bytes.extend_from_slice(&joint.to_le_bytes());
                }
                for weight in v.weights {
                    bytes.extend_from_slice(&weight.to_le_bytes());
                }
                // Where the rest pose stands it, which a blended primitive sorts by.
                let at = rig::skin(&rest, v.joints, v.weights, v.position);
                for c in 0..3 {
                    min[c] = min[c].min(at[c]);
                    max[c] = max[c].max(at[c]);
                }
            }
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-model-vertices"),
                contents: &bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_bytes: Vec<u8> = p.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-model-indices"),
                contents: &index_bytes,
                usage: wgpu::BufferUsages::INDEX,
            });
            primitives.push(GpuPrimitive {
                vertices,
                indices,
                index_count: u32::try_from(p.indices.len()).map_err(|_| RenderError::Invalid {
                    what: "model",
                    detail: "a primitive has more indices than a draw can take".into(),
                })?,
                material: p.material,
                center: [0, 1, 2].map(|c| (min[c] + max[c]) * 0.5),
            });
        }
        let bounds = model.bounds();
        Ok(GpuModel {
            inner: Arc::new(Uploaded {
                owner: self.owner.clone(),
                version: model.version(),
                bounds: SolidBounds {
                    min: bounds.min,
                    max: bounds.max,
                },
                primitives,
                materials,
                pose: (!rest.is_empty()).then(|| {
                    let (buffer, bind) = pose_bind(device, &self.pose_layout, &rest);
                    (buffer, bind, rest.len())
                }),
                live: Mutex::new(None),
                #[cfg(test)]
                formats,
            }),
        })
    }
}

impl ModelRuntime {
    /// Writes `palette` as `model`'s pose for its next draws (ADR-0151 decision 7). A
    /// model without a rig, or a palette of another length, is left as it is.
    pub(crate) fn pose(gpu: &Gpu, model: &GpuModel, palette: &[rig::Mat4]) {
        if let Some((buffer, _, slots)) = &model.inner.pose
            && *slots == palette.len()
        {
            let bytes: Vec<u8> = palette
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            gpu.queue.write_buffer(buffer, 0, &bytes);
        }
    }

    /// The frame bind group for a draw with shadow maps `maps`: the plain one with none,
    /// else the one kept for the same maps, made again when they change.
    /// A slot of `chains` binds that mip chain in place of its map.
    fn frame_bind_for(
        &mut self,
        gpu: &Gpu,
        maps: [Option<&Image>; FRAME_TEXTURES],
        chains: [Option<&(wgpu::Texture, wgpu::TextureView)>; FRAME_TEXTURES],
    ) -> wgpu::BindGroup {
        if maps.iter().all(Option::is_none) {
            return self.frame_bind.clone();
        }
        let textures: [Option<wgpu::Texture>; FRAME_TEXTURES] =
            std::array::from_fn(|n| match chains[n] {
                Some((texture, _)) => Some(texture.clone()),
                None => maps[n].map(|image| image.texture().clone()),
            });
        if let Some((kept, bind)) = &self.shadow_bind
            && *kept == textures
        {
            return bind.clone();
        }
        let views: [Option<wgpu::TextureView>; FRAME_TEXTURES] =
            std::array::from_fn(|n| match chains[n] {
                Some((_, view)) => Some(view.clone()),
                None => maps[n].map(|image| {
                    image
                        .texture()
                        .create_view(&wgpu::TextureViewDescriptor::default())
                }),
            });
        let view = |n: usize| views[n].as_ref().unwrap_or(&self.white);
        let bind = frame_bind_group(
            &gpu.device,
            &self.frame_layout,
            (&self.frame, &self.sampler),
            [view(0), view(1), view(2), view(3)],
        );
        self.shadow_bind = Some((textures, bind.clone()));
        bind
    }

    /// Draws `draw` into `target`, replacing what it held: transparent wherever the
    /// model isn't.
    /// `live` is `draw.texture`'s mip chain, its texture and its whole view, when its
    /// picture is drawing, and `panoramas` the chains its blurred reflections read.
    pub(crate) fn draw(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        draw: &ModelDraw<'_>,
        target: &Image,
        (live, panoramas): (Option<(&wgpu::Texture, &wgpu::TextureView)>, Panoramas<'_>),
        (surface, view_depth): (Option<[&Image; 3]>, Option<&Image>),
    ) -> Result<(), RenderError> {
        let how = surface.map_or(DrawAs::Layer, DrawAs::LayerWithSurface);
        self.draw_as(
            gpu,
            owner,
            draw,
            (target, view_depth),
            (live, panoramas),
            how,
        )
    }

    /// Merges a model's surface, drawn with its layer into `layers`, into `targets`, its
    /// scene's surface buffer, where it is nearer than what they hold since
    /// [`Self::clear_surface`] (ADR-0170 decision 4). Only the box the layer drew in is
    /// read, when it said which.
    pub(crate) fn merge_surface(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        layers: [&Image; 3],
        targets: [&Image; 3],
    ) -> Result<(), RenderError> {
        let views = surface_views(owner, targets)?;
        let (width, height) = (targets[0].width(), targets[0].height());
        let sources = surface_views(owner, layers)?;
        // The layers' size, which is the buffer's unless the model was drawn with fewer
        // pixels than its canvas (ADR-0197).
        let from = (layers[0].width(), layers[0].height());
        let depth = self.surface_depth_view(gpu, width, height);
        let (layout, pipeline) = &self.with_surface.merge;
        let read = |n: usize| wgpu::BindGroupEntry {
            binding: n as u32,
            resource: wgpu::BindingResource::TextureView(sources[n]),
        };
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-model-surface-merge"),
            layout,
            entries: &[read(0), read(1), read(2)],
        });
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model-surface-merge"),
            });
        {
            let keep = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model-surface-merge"),
                color_attachments: &[keep(views[0]), keep(views[1]), keep(views[2])],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(region) = layers[0].content() {
                if region.width == 0 || region.height == 0 {
                    drop(pass);
                    gpu.submit_work(encoder.finish());
                    drop(submitting);
                    return Ok(());
                }
                let region = spread(region, from, (width, height));
                pass.set_scissor_rect(region.x, region.y, region.width, region.height);
            }
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.submit_work(encoder.finish());
        for target in targets {
            target.mark_changed();
        }
        drop(submitting);
        Ok(())
    }

    /// The material bind groups `draw` binds with its live texture `live` (the clear
    /// stand-in for `None`), from those kept on its model when they match, else made
    /// and kept. Each is the material's own, its base colour's view replaced and its
    /// uniform marking that view as a premultiplied linear picture.
    fn live_binds(
        &self,
        gpu: &Gpu,
        model: &Uploaded,
        material: Option<&str>,
        live: Option<(&wgpu::Texture, &wgpu::TextureView)>,
    ) -> Vec<Option<(wgpu::BindGroup, wgpu::Buffer)>> {
        let (texture, view) = live.unwrap_or((&self.clear.0, &self.clear.1));
        let mut kept = model.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(binds) = kept.as_ref()
            && binds.texture == *texture
            && binds.material.as_deref() == material
        {
            return binds.binds.clone();
        }
        let binds: Vec<Option<(wgpu::BindGroup, wgpu::Buffer)>> = model
            .materials
            .iter()
            .map(|m| {
                if material.is_some_and(|name| name != m.name) {
                    return None;
                }
                let mut values = m.values;
                // The material's base colour texture is a source's picture.
                values[17] = 1.0;
                let uniform = gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("jstream-render-model-live-material"),
                        contents: &f32_bytes(&values),
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    });
                let mut entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: DETAIL_SAMPLER_BINDING,
                        resource: wgpu::BindingResource::Sampler(&self.detail_sampler),
                    },
                ];
                for (i, own) in m.views.iter().enumerate() {
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2 + i as u32,
                        resource: wgpu::BindingResource::TextureView(if i == 0 {
                            view
                        } else {
                            own
                        }),
                    });
                }
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("jstream-render-model-live-material"),
                    layout: &self.material_layout,
                    entries: &entries,
                });
                Some((bind, uniform))
            })
            .collect();
        *kept = Some(LiveBinds {
            texture: texture.clone(),
            material: material.map(str::to_owned),
            binds: binds.clone(),
        });
        binds
    }

    /// Lights `card.image`, a card's picture flat in its box layer, into `target`, a
    /// layer of the same size (ADR-0151 decision 10): each texel shaded where it sits
    /// in the scene, as a flat glTF quad there would be, and laid back where it was.
    /// `panoramas` are the chains its blurred reflections read.
    pub(crate) fn light_card(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        card: &CardDraw<'_>,
        target: &Image,
        panoramas: Panoramas<'_>,
    ) -> Result<(), RenderError> {
        if !card.image.owner().is(owner) {
            return Err(RenderError::Invalid {
                what: "image",
                detail: "the card's picture is not one this compositor made".into(),
            });
        }
        let device = &gpu.device;
        let finite = |value: f32, low: f32, high: f32| {
            if value.is_finite() {
                value.clamp(low, high)
            } else {
                low
            }
        };
        let emissive = finite(card.emissive, 0.0, 4.0);
        let values: [f32; 20] = [
            1.0,
            1.0,
            1.0,
            1.0,
            emissive,
            emissive,
            emissive,
            0.0,
            finite(card.metalness, 0.0, 1.0),
            finite(card.roughness, 0.0, 1.0),
            1.0,
            0.0,
            // Blended, double-sided and lit: the camera's side of it is shaded.
            2.0,
            0.0,
            1.0,
            0.0,
            // Its picture is premultiplied linear light, and emits as itself.
            1.0,
            0.0,
            0.0,
            0.0,
        ];
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("jstream-render-card-material"),
            contents: &f32_bytes(&values),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let picture = card
            .image
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let views = [
            &picture,
            &self.white,
            &self.flat_normal,
            &self.white,
            &picture,
        ];
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
            wgpu::BindGroupEntry {
                binding: DETAIL_SAMPLER_BINDING,
                resource: wgpu::BindingResource::Sampler(&self.detail_sampler),
            },
        ];
        for (i, view) in views.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: 2 + i as u32,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("jstream-render-card-material"),
            layout: &self.material_layout,
            entries: &entries,
        });
        let views: [wgpu::TextureView; 5] = views.map(Clone::clone);
        let (vertices, indices) = self.card_quad(gpu);
        let model = GpuModel {
            inner: Arc::new(Uploaded {
                owner: owner.clone(),
                version: 0,
                bounds: CARD_BOUNDS,
                primitives: vec![GpuPrimitive {
                    vertices,
                    indices,
                    index_count: 6,
                    material: 0,
                    center: [0.0; 3],
                }],
                materials: vec![GpuMaterial {
                    bind,
                    // Drawn as it is: its texels replace the cleared layer's.
                    blended: false,
                    name: String::new(),
                    values,
                    uniform,
                    driven: std::sync::atomic::AtomicBool::new(false),
                    live_driven: std::sync::atomic::AtomicBool::new(false),
                    views,
                }],
                pose: None,
                live: Mutex::new(None),
                #[cfg(test)]
                formats: Vec::new(),
            }),
        };
        let draw = ModelDraw {
            model: &model,
            geometry: ItemGeometry {
                fit_mode: jstream_protocol::FitMode::Stretch,
                flip_h: false,
                flip_v: false,
                ..card.geometry
            },
            camera: Some(card.camera),
            lighting: Lighting::Scene,
            lights: Some(card.lights),
            shadows: card.shadows,
            environment: card.environment,
            floor: None,
            materials: &[],
            texture: None,
            reflection: None,
            probe: card.probe,
        };
        self.draw_as(
            gpu,
            owner,
            &draw,
            (target, None),
            (None, panoramas),
            DrawAs::InTexture,
        )
    }

    /// The quad a card is lit on: a unit square in its own plane, facing the viewer,
    /// made once.
    fn card_quad(&mut self, gpu: &Gpu) -> (wgpu::Buffer, wgpu::Buffer) {
        if let Some(quad) = &self.card {
            return quad.clone();
        }
        // Top left, top right, bottom right, bottom left: clockwise on the y-down canvas.
        let corners = [[-0.5f32, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
        let mut bytes = Vec::with_capacity(4 * VERTEX_STRIDE as usize);
        for [x, y] in corners {
            let vertex = [
                x,
                y,
                0.0,
                0.0,
                0.0,
                1.0,
                1.0,
                0.0,
                0.0,
                1.0,
                x + 0.5,
                y + 0.5,
                1.0,
                1.0,
                1.0,
                1.0,
            ];
            bytes.extend(f32_bytes(&vertex));
            // No joints and no weights: it stands where it is.
            bytes.extend([0u8; 8]);
            bytes.extend(f32_bytes(&[0.0; 4]));
        }
        let vertices = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-card-vertices"),
                contents: &bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_bytes: Vec<u8> = [0u32, 1, 2, 0, 2, 3]
            .iter()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        let indices = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("jstream-render-card-indices"),
                contents: &index_bytes,
                usage: wgpu::BufferUsages::INDEX,
            });
        self.card = Some((vertices.clone(), indices.clone()));
        (vertices, indices)
    }

    /// Draws `draw` into `target`: through its camera, or, [`DrawAs::InTexture`], laid
    /// flat over the target in its own texture coordinates while shaded where it sits in
    /// the scene, its camera's side of it facing. [`DrawAs::Surface`] draws what a
    /// screen-space reflection reads instead, into its targets, `target` the first.
    fn draw_as(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        draw: &ModelDraw<'_>,
        (target, view_depth): (&Image, Option<&Image>),
        (live, panoramas): (Option<(&wgpu::Texture, &wgpu::TextureView)>, Panoramas<'_>),
        how: DrawAs<'_>,
    ) -> Result<(), RenderError> {
        let in_texture = matches!(how, DrawAs::InTexture);
        let model = &draw.model.inner;
        if !model.owner.is(owner) {
            return Err(RenderError::Invalid {
                what: "model",
                detail: "the model was uploaded by another compositor".into(),
            });
        }
        let Some(view) = target.target().filter(|_| target.owner().is(owner)) else {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: "the target is not a layer this compositor made".into(),
            });
        };
        let (width, height) = (target.width(), target.height());
        // Its view depth, read from the depth the layer's pass tests rather than drawn
        // again: a depth layer this compositor made, of the layer's size.
        let view_depth = match view_depth {
            Some(depth) => Some(
                depth
                    .depth_target()
                    .filter(|_| {
                        depth.owner().is(owner)
                            && (depth.width(), depth.height()) == (width, height)
                    })
                    .map(|view| (depth, view))
                    .ok_or_else(|| RenderError::Invalid {
                        what: "layer",
                        detail: "the view depth is not a depth layer this compositor made \
                                 of the model's layer's size"
                            .into(),
                    })?,
            ),
            None => None,
        };
        // The canvas the model is placed on is the target's extent: a layer drawn with
        // fewer pixels than its canvas (ADR-0197) holds the same picture, less sharp.
        let canvas = canvas_of(target);
        let camera = match draw.camera {
            Some(camera) => Some(camera.view(canvas).map_err(placement)?),
            None => None,
        };
        let solid = draw
            .geometry
            .solid(model.bounds, camera.as_ref())
            .map_err(placement)?;
        // Orthographic depth: scaled so the model's own depth stays well inside 0..1.
        let reach = (0..8)
            .map(|i| {
                let pick = |bit: usize, axis: usize| {
                    if i & (1 << bit) == 0 {
                        model.bounds.min[axis]
                    } else {
                        model.bounds.max[axis]
                    }
                };
                solid.in_view([pick(0, 0), pick(1, 1), pick(2, 2)])[2].abs()
            })
            .fold(1.0f64, f64::max);
        let (clip, mirrored) = if in_texture {
            // A model point's x and y, from -0.5 to 0.5, across the whole target; the
            // quad winds as a front face, so the shader's mirroring picks the side.
            let clip = [
                [2.0, 0.0, 0.0, 0.0],
                [0.0, -2.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.5, 1.0],
            ];
            (clip, !sees_front(&solid))
        } else {
            (solid.clip_matrix(canvas, 0.45 / reach), solid.mirrored())
        };
        let lights = match (draw.lights, camera.as_ref()) {
            (Some(lights), Some(view)) if draw.lighting != Lighting::Unlit => {
                Some(SceneLighting::new(lights, view))
            }
            _ => None,
        };
        // The shadow maps of the lights that shade it, that this compositor drew.
        let mut views = [[0.0f32; 16]; MAX_SHADOW_MAPS];
        let mut maps: [Option<&Image>; FRAME_TEXTURES] = [None; FRAME_TEXTURES];
        let mut count = 0;
        if let (Some(lighting), Some(view)) = (lights.as_ref(), camera.as_ref()) {
            for shadow in draw.shadows {
                if count == MAX_SHADOW_MAPS || !shadow.image.owner().is(owner) {
                    continue;
                }
                if let Some(seen) = shadow_view(shadow, lighting, view) {
                    views[count] = seen;
                    maps[count] = Some(shadow.image);
                    count += 1;
                }
            }
        }
        // What the model reflects (ADR-0107 decision 5), in place of the studio
        // gradient or its scene's sky: a picture seen as a panorama about the viewing
        // frame, unscaled by any ambient light unless the scene has one.
        let reflection = draw
            .reflection
            .filter(|image| image.owner().is(owner) && draw.lighting != Lighting::Unlit);
        // The scene's sky, for a model lit by its scene (ADR-0151 decision 9).
        let mut sky = match (
            reflection,
            draw.environment,
            lights.as_ref(),
            camera.as_ref(),
        ) {
            (Some(image), ..) => {
                maps[MAX_SHADOW_MAPS] = Some(image);
                Some(reflection_values(image, lights.as_ref(), camera.as_ref()))
            }
            (None, Some(environment), Some(lighting), Some(view)) => {
                let image = environment.image.filter(|image| image.owner().is(owner));
                maps[MAX_SHADOW_MAPS] = image;
                Some(environment_values(&environment, image, lighting, view))
            }
            _ => None,
        };
        // The reflection probe a model lit by its scene reads (ADR-0170 decision 3).
        let mut probe = match (draw.probe, lights.as_ref(), camera.as_ref()) {
            (Some(probe), Some(_), Some(view))
                if probe.image.owner().is(owner) && draw.lighting == Lighting::Scene =>
            {
                maps[PROBE_SLOT] = Some(probe.image);
                Some(probe_values(&probe, view))
            }
            _ => None,
        };
        // A picture read through its mip chain reads premultiplied linear light.
        let mut chains = [None; FRAME_TEXTURES];
        for (slot, chain) in [
            (MAX_SHADOW_MAPS, panoramas.sky),
            (PROBE_SLOT, panoramas.probe),
        ] {
            chains[slot] = maps[slot].and_then(|image| Panoramas::of(chain, image));
        }
        let premultiplied = crate::image::Alpha::Premultiplied.shader_mode();
        if let (Some(values), Some(_)) = (sky.as_mut(), chains[MAX_SHADOW_MAPS]) {
            values[SKY_MODE] = premultiplied;
        }
        if let (Some(values), Some(_)) = (probe.as_mut(), chains[PROBE_SLOT]) {
            values[PROBE_MODE] = premultiplied;
        }
        let frame = frame_bytes(
            (&solid, mirrored),
            &clip,
            draw.lighting,
            lights.as_ref(),
            (
                &views[..count],
                sky.as_ref(),
                floor_cut(draw.floor, camera.as_ref()),
                probe.as_ref(),
            ),
        );
        let frame_bind = self.frame_bind_for(gpu, maps, chains);
        let live_binds = draw
            .texture
            .map(|texture| self.live_binds(gpu, model, texture.material, live));
        let beside = match how {
            DrawAs::LayerWithSurface(layers) => {
                let views = surface_views(owner, layers)?;
                if (layers[0].width(), layers[0].height()) != (width, height) {
                    return Err(RenderError::Invalid {
                        what: "layer",
                        detail: "a model's surface layers are not its layer's size".into(),
                    });
                }
                Some((layers, views))
            }
            _ => None,
        };
        // A lit card's quad covers its whole target, with no edge to smooth (ADR-0242).
        let samples = if in_texture { 1 } else { self.samples };
        let multi = (samples > 1).then(|| {
            let resolve = beside.as_ref().map(|_| &self.surface_resolve);
            self.msaa.get(gpu, ((width, height), samples), resolve)
        });
        let depth = match &multi {
            Some(multi) => multi.depth.clone(),
            None => self.depth_view(gpu, width, height),
        };
        let at = self.pipelines_at(gpu, samples);
        // Blended primitives far to near by their centres.
        self.order.clear();
        for (index, p) in model.primitives.iter().enumerate() {
            if model.materials.get(p.material).is_some_and(|m| m.blended) {
                self.order.push((solid.in_view(p.center)[2], index));
            }
        }
        self.order
            .sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        gpu.queue.write_buffer(&self.frame, 0, &frame);
        // This draw's material drives (ADR-0171 decision 4), and the last draw's put
        // back: each draw submits on its own, so a write lands before its draw only.
        // A material with a live texture binds its own uniform (ADR-0107 decision 5),
        // driven the same way.
        for (index, m) in model.materials.iter().enumerate() {
            use std::sync::atomic::Ordering;
            let driven = driven_values(&m.values, &m.name, draw.materials);
            match driven {
                Some(values) => {
                    gpu.queue
                        .write_buffer(&m.uniform, 0, &material_bytes(&values));
                    m.driven.store(true, Ordering::Relaxed);
                }
                None => {
                    if m.driven.swap(false, Ordering::Relaxed) {
                        gpu.queue
                            .write_buffer(&m.uniform, 0, &material_bytes(&m.values));
                    }
                }
            }
            let Some((_, live)) = live_binds
                .as_ref()
                .and_then(|binds| binds.get(index)?.as_ref())
            else {
                continue;
            };
            let was = m.live_driven.swap(driven.is_some(), Ordering::Relaxed);
            if driven.is_some() || was {
                let mut values = driven.unwrap_or(m.values);
                // The base colour texture is a source's picture, as `live_binds` marks it.
                values[17] = 1.0;
                gpu.queue.write_buffer(live, 0, &material_bytes(&values));
            }
        }
        // A surface on its own reads the same materials, live textures and drives.
        if let DrawAs::Surface(targets) = how {
            let drawn = self.draw_surface(
                gpu,
                owner,
                (model, &frame_bind, live_binds.as_deref()),
                targets,
            );
            drop(submitting);
            return drawn;
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model"),
            });
        {
            let cleared = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            // Multisampled (ADR-0242), the colour's samples resolve into the layer as
            // the pass ends and are not kept; the surface's are kept for its resolve.
            let colour = match &multi {
                Some(multi) => Some(wgpu::RenderPassColorAttachment {
                    view: &multi.colour,
                    depth_slice: None,
                    resolve_target: Some(view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                }),
                None => cleared(view),
            };
            let surface_views = match (&multi, &beside) {
                (
                    Some(Drawn {
                        surface: Some((views, _)),
                        ..
                    }),
                    Some(_),
                ) => Some([&views[0], &views[1], &views[2]]),
                (_, Some((_, views))) => Some(*views),
                _ => None,
            };
            let attachments = match surface_views {
                Some(views) => [
                    colour,
                    cleared(views[0]),
                    cleared(views[1]),
                    cleared(views[2]),
                ],
                None => [colour, None, None, None],
            };
            let attachments = if beside.is_some() {
                &attachments[..]
            } else {
                &attachments[..1]
            };
            let pipelines = &self.layer[at].1;
            let [prepass, opaque, blended] = match beside {
                Some(_) => &pipelines.surface,
                None => &pipelines.plain,
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model"),
                color_attachments: attachments,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        // Kept only for the view depth to be read from.
                        store: if view_depth.is_some() && camera.is_some() {
                            wgpu::StoreOp::Store
                        } else {
                            wgpu::StoreOp::Discard
                        },
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &frame_bind, &[]);
            pass.set_bind_group(
                2,
                model.pose.as_ref().map_or(&self.rest_pose, |p| &p.1),
                &[],
            );
            let draw_one = |pass: &mut wgpu::RenderPass<'_>, p: &GpuPrimitive| {
                let Some(material) = model.materials.get(p.material) else {
                    return;
                };
                let bind = live_binds
                    .as_ref()
                    .and_then(|binds| binds.get(p.material)?.as_ref())
                    .map_or(&material.bind, |(bind, _)| bind);
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, p.vertices.slice(..));
                pass.set_index_buffer(p.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..p.index_count, 0, 0..1);
            };
            // Depth first, then shading only where it is the nearest: a pixel several
            // surfaces cover is shaded once, not once a surface.
            let is_opaque =
                |p: &&GpuPrimitive| !model.materials.get(p.material).is_some_and(|m| m.blended);
            pass.set_pipeline(prepass);
            for p in model.primitives.iter().filter(is_opaque) {
                draw_one(&mut pass, p);
            }
            // Last to first: where two primitives lie in one plane both pass the equal
            // test and the later draw replaces the earlier, so this order shows the
            // first of them, as one pass that kept the first nearest fragment did.
            // Inside one primitive its triangles still draw in index order, so of two
            // of its own triangles in one plane the later now shows.
            pass.set_pipeline(opaque);
            for p in model.primitives.iter().rev().filter(is_opaque) {
                draw_one(&mut pass, p);
            }
            if !self.order.is_empty() {
                pass.set_pipeline(blended);
                for &(_, index) in &self.order {
                    draw_one(&mut pass, &model.primitives[index]);
                }
            }
        }
        if let (
            Some(Drawn {
                surface: Some((_, bind)),
                ..
            }),
            Some((_, views)),
        ) = (&multi, &beside)
        {
            self.surface_resolve.record(&mut encoder, bind, *views);
        }
        if let Some((_, depth_view)) = view_depth {
            self.record_view_depth(
                gpu,
                &mut encoder,
                (model, camera.is_some()),
                (&depth, samples),
                depth_view,
            );
        }
        gpu.submit_work(encoder.finish());
        target.mark_changed();
        // Outside the model's box on the target the layer is transparent: what places
        // or filters it draws only over the rest (#1070).
        let content = (!in_texture && model.pose.is_none())
            .then(|| drawn_box(&clip, model.bounds, (width, height)))
            .flatten();
        target.set_content(content);
        if let Some((layers, _)) = beside {
            for layer in layers {
                layer.mark_changed();
                layer.set_content(content);
            }
        }
        // Its view depth holds the model where the layer does, and nothing elsewhere.
        if let Some((depth, _)) = view_depth {
            depth.mark_changed();
            depth.set_content(content);
        }
        drop(submitting);
        Ok(())
    }

    /// Records the view depth of the model whose layer's pass just tested `depth` into
    /// `target` (ADR-0143 decision 4), as [`Self::draw_depth`] would draw it: the
    /// opaque primitives' nearest depth read from the tested one, then the blended
    /// primitives at least half opaque drawn over it, the nearest kept, since the layer's
    /// pass tests them but writes no depth for them. Seen without a camera it holds
    /// nothing, as `draw_depth` draws nothing.
    fn record_view_depth(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        (model, seen): (&Uploaded, bool),
        (depth, samples): (&wgpu::TextureView, u32),
        target: &wgpu::TextureView,
    ) {
        if !seen {
            let _cleared = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model-view-depth-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            return;
        }
        self.depth_resolve
            .record(gpu, encoder, (depth, samples), target);
        if self.order.is_empty() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("jstream-render-model-view-depth-blended"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.depth_only);
        pass.set_bind_group(0, &self.frame_bind, &[]);
        pass.set_bind_group(
            2,
            model.pose.as_ref().map_or(&self.rest_pose, |p| &p.1),
            &[],
        );
        for &(_, index) in &self.order {
            let p = &model.primitives[index];
            let Some(material) = model.materials.get(p.material) else {
                continue;
            };
            pass.set_bind_group(1, &material.bind, &[]);
            pass.set_vertex_buffer(0, p.vertices.slice(..));
            pass.set_index_buffer(p.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..p.index_count, 0, 0..1);
        }
    }

    /// Adds `draw`'s view depth to `target` (ADR-0143 decision 4), keeping what is there
    /// where it is nearer. Nothing without a camera.
    pub(crate) fn draw_depth(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        draw: &ModelDraw<'_>,
        target: &Image,
    ) -> Result<(), RenderError> {
        let model = &draw.model.inner;
        if !model.owner.is(owner) {
            return Err(RenderError::Invalid {
                what: "model",
                detail: "the model was uploaded by another compositor".into(),
            });
        }
        let Some(view) = target.depth_target().filter(|_| target.owner().is(owner)) else {
            return Err(RenderError::Invalid {
                what: "layer",
                detail: "the target is not a depth layer this compositor made".into(),
            });
        };
        let Some(camera) = draw.camera else {
            return Ok(());
        };
        let canvas = canvas_of(target);
        let camera = camera.view(canvas).map_err(placement)?;
        let solid = draw
            .geometry
            .solid(model.bounds, Some(&camera))
            .map_err(placement)?;
        // With perspective the orthographic depth scale is unused.
        let clip = solid.clip_matrix(canvas, 0.0);
        let frame = frame_bytes(
            (&solid, solid.mirrored()),
            &clip,
            Lighting::Unlit,
            None,
            (&[], None, floor_cut(draw.floor, Some(&camera)), None),
        );
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        gpu.queue.write_buffer(&self.frame, 0, &frame);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model-view-depth"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model-view-depth"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.depth_only);
            pass.set_bind_group(0, &self.frame_bind, &[]);
            pass.set_bind_group(
                2,
                model.pose.as_ref().map_or(&self.rest_pose, |p| &p.1),
                &[],
            );
            for p in &model.primitives {
                let Some(material) = model.materials.get(p.material) else {
                    continue;
                };
                pass.set_bind_group(1, &material.bind, &[]);
                pass.set_vertex_buffer(0, p.vertices.slice(..));
                pass.set_index_buffer(p.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..p.index_count, 0, 0..1);
            }
        }
        gpu.submit_work(encoder.finish());
        // The target holds what it held and the model's box more.
        let prior = target.content();
        target.mark_changed();
        let content = match (prior, model.pose.is_none()) {
            (Some(prior), true) => {
                drawn_box(&clip, model.bounds, (target.width(), target.height()))
                    .map(|drawn| union(prior, drawn))
            }
            _ => None,
        };
        target.set_content(content);
        drop(submitting);
        Ok(())
    }

    /// Clears `targets`, a screen-space reflection's surface buffer (ADR-0170 decision
    /// 4), and the depth its models test against, before [`Self::draw_surface_of`] draws
    /// each model into them.
    pub(crate) fn clear_surface(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        targets: [&Image; 3],
    ) -> Result<(), RenderError> {
        let views = surface_views(owner, targets)?;
        let (width, height) = (targets[0].width(), targets[0].height());
        let depth = self.surface_depth_view(gpu, width, height);
        let lock = gpu.submit.clone();
        let submitting = lock.lock();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model-surface-clear"),
            });
        {
            let clear = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model-surface-clear"),
                color_attachments: &[clear(views[0]), clear(views[1]), clear(views[2])],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        gpu.submit_work(encoder.finish());
        for target in targets {
            target.mark_changed();
            target.set_content(None);
        }
        drop(submitting);
        Ok(())
    }

    /// Draws `draw`'s surface into `targets` (ADR-0170 decision 4), shaded as
    /// [`Self::draw`] shades it, where it is nearer than what the targets hold since
    /// [`Self::clear_surface`]. Nothing without a camera.
    pub(crate) fn draw_surface_of(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        draw: &ModelDraw<'_>,
        (targets, (live, panoramas)): ([&Image; 3], (LiveView<'_>, Panoramas<'_>)),
    ) -> Result<(), RenderError> {
        if draw.camera.is_none() {
            return Ok(());
        }
        self.draw_as(
            gpu,
            owner,
            draw,
            (targets[0], None),
            (live, panoramas),
            DrawAs::Surface(targets),
        )
    }

    /// The surface pass of [`Self::draw_as`], which holds the submit lock and wrote the
    /// frame and the materials' drives: every primitive, the nearest kept, each bound as
    /// the layer binds it (its live texture's group where it has one).
    fn draw_surface(
        &mut self,
        gpu: &Gpu,
        owner: &Owner,
        (model, frame_bind, live_binds): (&Uploaded, &wgpu::BindGroup, Option<&[LiveBind]>),
        targets: [&Image; 3],
    ) -> Result<(), RenderError> {
        let views = surface_views(owner, targets)?;
        let (width, height) = (targets[0].width(), targets[0].height());
        let depth = self.surface_depth_view(gpu, width, height);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jstream-render-model-surface"),
            });
        {
            let keep = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jstream-render-model-surface"),
                color_attachments: &[keep(views[0]), keep(views[1]), keep(views[2])],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.surface);
            pass.set_bind_group(0, frame_bind, &[]);
            pass.set_bind_group(
                2,
                model.pose.as_ref().map_or(&self.rest_pose, |p| &p.1),
                &[],
            );
            for p in &model.primitives {
                let Some(material) = model.materials.get(p.material) else {
                    continue;
                };
                let bind = live_binds
                    .and_then(|binds| binds.get(p.material)?.as_ref())
                    .map_or(&material.bind, |(bind, _)| bind);
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, p.vertices.slice(..));
                pass.set_index_buffer(p.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..p.index_count, 0, 0..1);
            }
        }
        gpu.submit_work(encoder.finish());
        for target in targets {
            target.mark_changed();
        }
        Ok(())
    }

    /// The depth a surface buffer's models test against, kept between
    /// [`Self::clear_surface`] and the draws after it; made again for another size.
    fn surface_depth_view(&mut self, gpu: &Gpu, width: u32, height: u32) -> wgpu::TextureView {
        if let Some((w, h, view)) = &self.surface_depth
            && (*w, *h) == (width, height)
        {
            return view.clone();
        }
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-model-surface-depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.surface_depth = Some((width, height, view.clone()));
        view
    }

    /// Sets the samples a model's layer is drawn with from now on (ADR-0242): the most
    /// no more than `samples` this device takes, 1 drawing it without anti-aliasing.
    /// Returns the count it draws with.
    pub(crate) fn set_samples(&mut self, gpu: &Gpu, samples: u32) -> u32 {
        self.samples = msaa::supported(gpu, samples);
        self.samples
    }

    /// The samples a model's layer is drawn with, as [`Self::set_samples`] last set it.
    pub(crate) fn samples(&self) -> u32 {
        self.samples
    }

    /// Where [`Self::layer`] holds the pipelines drawn with `samples`, made first if
    /// they aren't yet.
    fn pipelines_at(&mut self, gpu: &Gpu, samples: u32) -> usize {
        if let Some(at) = self.layer.iter().position(|(n, _)| *n == samples) {
            return at;
        }
        let made = layer_pipelines(gpu, (&self.shader, &self.layout), samples);
        self.layer.push((samples, made));
        self.layer.len() - 1
    }

    fn depth_view(&mut self, gpu: &Gpu, width: u32, height: u32) -> wgpu::TextureView {
        if let Some(at) = self
            .depth
            .iter()
            .position(|(w, h, _)| (*w, *h) == (width, height))
        {
            // The latest used is kept first.
            self.depth[..=at].rotate_right(1);
            return self.depth[0].2.clone();
        }
        // A pass's attachments must all be one size, so a layer of another size makes a
        // new one (a canvas resize, or a second canvas through the same compositor).
        // The two sizes used last are kept: a model drawn with fewer pixels than its
        // canvas (ADR-0197) alternates, every frame, with what is drawn at the canvas's.
        let (w, h) = (width, height);
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-render-model-depth"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            // Read for the model's view depth ([`Self::record_view_depth`]).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth.truncate(KEPT_DEPTHS - 1);
        self.depth.insert(0, (w, h, view.clone()));
        view
    }
}

/// `region` of a layer `from` pixels in size, as the texels it covers of one `to` pixels
/// in size that shows the same picture: itself when the two are one size, else grown to
/// whole texels and kept inside `to`.
fn spread(region: Region, from: (u32, u32), to: (u32, u32)) -> Region {
    if from == to || from.0 == 0 || from.1 == 0 {
        return region;
    }
    let low = |at: u32, from: u32, to: u32| {
        (u64::from(at) * u64::from(to) / u64::from(from)).min(u64::from(to)) as u32
    };
    let high = |at: u32, from: u32, to: u32| {
        (u64::from(at) * u64::from(to))
            .div_ceil(u64::from(from))
            .min(u64::from(to)) as u32
    };
    let x = low(region.x, from.0, to.0);
    let y = low(region.y, from.1, to.1);
    Region {
        x,
        y,
        width: high(region.x.saturating_add(region.width), from.0, to.0).saturating_sub(x),
        height: high(region.y.saturating_add(region.height), from.1, to.1).saturating_sub(y),
    }
}

/// The canvas a model drawn into `target` is placed on: the target's extent
/// ([`Image::extent`]), which is its size in pixels unless it stands for a larger canvas
/// (ADR-0197).
pub(crate) fn canvas_of(target: &Image) -> Canvas {
    let (width, height) = target.extent();
    Canvas { width, height }
}

/// The model pipelines that draw a layer and its surface at once, and the merge of that
/// surface into its scene's surface buffer (ADR-0170 decision 4).
struct WithSurface {
    merge: (wgpu::BindGroupLayout, wgpu::RenderPipeline),
}

/// The layer's pipelines at one sample count: the opaque primitives' depth, drawn
/// before their shading, their shading where the prepass left their depth, and the
/// blended ones; on their own, and with a surface buffer beside the layer.
struct LayerPipelines {
    plain: [wgpu::RenderPipeline; 3],
    surface: [wgpu::RenderPipeline; 3],
}

/// The layer's pipelines drawn with `samples` (ADR-0242), of the model shader and its
/// pipeline layout.
fn layer_pipelines(
    gpu: &Gpu,
    (shader, layout): (&wgpu::ShaderModule, &wgpu::PipelineLayout),
    samples: u32,
) -> LayerPipelines {
    let device = &gpu.device;
    // The opaque primitives draw twice: their depth first (`fs_prepass`), then their
    // shading where it is equal, which writes no depth; blended ones test it.
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Prepass,
        Opaque,
        Blended,
    }
    // With `surface`, the pass also has a surface buffer's three targets, which only
    // the opaque shading writes (`fs_with_surface`, ADR-0170 decision 4).
    let pipeline = |kind: Kind, surface: bool| {
        let unwritten = Some(wgpu::ColorTargetState {
            format: LAYER_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::empty(),
        });
        let written = Some(wgpu::ColorTargetState {
            format: LAYER_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        });
        let beside = if kind == Kind::Opaque {
            written
        } else {
            unwritten
        };
        let targets = [
            Some(wgpu::ColorTargetState {
                format: LAYER_FORMAT,
                blend: (kind != Kind::Prepass)
                    .then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: if kind == Kind::Prepass {
                    wgpu::ColorWrites::empty()
                } else {
                    wgpu::ColorWrites::ALL
                },
            }),
            beside.clone(),
            beside.clone(),
            beside,
        ];
        let desc = wgpu::RenderPipelineDescriptor {
            label: Some(match (kind, surface) {
                (Kind::Prepass, false) => "jstream-render-model-prepass",
                (Kind::Opaque, false) => "jstream-render-model-opaque",
                (Kind::Blended, false) => "jstream-render-model-blended",
                (Kind::Prepass, true) => "jstream-render-model-prepass-surface",
                (Kind::Opaque, true) => "jstream-render-model-opaque-surface",
                (Kind::Blended, true) => "jstream-render-model-blended-surface",
            }),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x4,
                        3 => Float32x2,
                        4 => Float32x4,
                        5 => Uint16x4,
                        6 => Float32x4,
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // A front face winds clockwise on the y-down canvas, which is
                // clockwise in clip space too. The shader discards back faces, since
                // a mirrored placement swaps them.
                front_face: wgpu::FrontFace::Cw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(kind == Kind::Prepass),
                depth_compare: Some(if kind == Kind::Opaque {
                    wgpu::CompareFunction::Equal
                } else {
                    wgpu::CompareFunction::Greater
                }),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(match kind {
                    Kind::Prepass => "fs_prepass",
                    Kind::Opaque if surface => "fs_with_surface",
                    _ => "fs",
                }),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // The prepass draws in the colour pass, so it has its target, and
                // writes none of it.
                targets: if surface { &targets[..] } else { &targets[..1] },
            }),
            multiview_mask: None,
            cache: None,
        };
        gpu.built.render(device, SOURCE, &(), &desc)
    };
    LayerPipelines {
        plain: [
            pipeline(Kind::Prepass, false),
            pipeline(Kind::Opaque, false),
            pipeline(Kind::Blended, false),
        ],
        surface: [
            pipeline(Kind::Prepass, true),
            pipeline(Kind::Opaque, true),
            pipeline(Kind::Blended, true),
        ],
    }
}

/// The pipeline of `surface_merge.wgsl` and the layout of its three layers.
fn merge_pipeline(gpu: &Gpu) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline) {
    const SOURCE: &str = include_str!("surface_merge.wgsl");
    let device = &gpu.device;
    let entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("jstream-render-model-surface-merge"),
        entries: &[entry(0), entry(1), entry(2)],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("jstream-render-model-surface-merge"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("jstream-render-model-surface-merge"),
        source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
    });
    let plain = Some(wgpu::ColorTargetState {
        format: LAYER_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    });
    let desc = wgpu::RenderPipelineDescriptor {
        label: Some("jstream-render-model-surface-merge"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[plain.clone(), plain.clone(), plain],
        }),
        multiview_mask: None,
        cache: None,
    };
    let pipeline = gpu.built.render(device, SOURCE, &(), &desc);
    (layout, pipeline)
}

/// A live texture's whole picture as a model reads it: its texture and its view.
type LiveView<'a> = Option<(&'a wgpu::Texture, &'a wgpu::TextureView)>;

/// A material's bind group for a live texture, with its uniform, where it has one.
type LiveBind = Option<(wgpu::BindGroup, wgpu::Buffer)>;

/// What [`ModelRuntime::draw_as`] draws.
#[derive(Clone, Copy)]
enum DrawAs<'a> {
    /// The model through its camera, into a layer.
    Layer,
    /// The model through its camera, into a layer, and its surface into these three
    /// layers of the layer's size, cleared first (ADR-0170 decision 4).
    LayerWithSurface([&'a Image; 3]),
    /// A lit card's quad, laid flat over the layer in its texture coordinates.
    InTexture,
    /// What a screen-space reflection reads of it, into these three targets
    /// (ADR-0170 decision 4).
    Surface([&'a Image; 3]),
}

/// The views of a surface buffer's three `targets`: layers `owner`'s compositor made, all
/// one size.
fn surface_views<'t>(
    owner: &Owner,
    targets: [&'t Image; 3],
) -> Result<[&'t wgpu::TextureView; 3], RenderError> {
    let size = (targets[0].width(), targets[0].height());
    let view = |target: &'t Image| {
        target
            .target()
            .filter(|_| target.owner().is(owner) && (target.width(), target.height()) == size)
            .ok_or_else(|| RenderError::Invalid {
                what: "layer",
                detail: "a surface target is not a layer this compositor made, of one size".into(),
            })
    };
    Ok([view(targets[0])?, view(targets[1])?, view(targets[2])?])
}

fn placement(err: jstream_protocol::GeometryError) -> RenderError {
    RenderError::Invalid {
        what: "placement",
        detail: err.to_string(),
    }
}

/// A scene's lights in the shader's terms: the model's viewing frame, linear light.
pub(crate) struct SceneLighting {
    pub(crate) ambient: [f32; 3],
    /// Four `vec4` per light, as `model.wgsl`'s `Light`.
    pub(crate) lights: Vec<[f32; 16]>,
    /// For each of `lights`, its index in the lights it was made from.
    pub(crate) sources: Vec<usize>,
    /// Whether an ambient light is among them, at any intensity: it then scales the
    /// environment, which otherwise lights a model on its own (ADR-0151 decision 9).
    pub(crate) has_ambient: bool,
}

impl SceneLighting {
    pub(crate) fn new(lights: &[SceneLight], view: &jstream_protocol::CameraView) -> Self {
        let mut ambient = [0.0f32; 3];
        let mut has_ambient = false;
        let mut packed = Vec::with_capacity(MAX_SCENE_LIGHTS);
        let mut sources = Vec::with_capacity(MAX_SCENE_LIGHTS);
        // Slots taken by the shading lights so far, lit or not.
        let mut slots = 0;
        for (source, light) in lights.iter().enumerate() {
            let positive = |v: f32| v.is_finite() && v > 0.0;
            // An ambient light scales the environment even faded out to nothing.
            has_ambient |= light.kind == LightKind::Ambient;
            if light.kind != LightKind::Ambient {
                // Every shading light takes one of the 8 in order, lit or not, so fading
                // one of them out never switches a ninth on (ADR-0143 decision 3).
                slots += 1;
                if slots > MAX_SCENE_LIGHTS {
                    continue;
                }
            }
            // Unlit, or a value no settings check would let through: lights nothing.
            let placed = light.kind == LightKind::Directional
                || light.kind == LightKind::Ambient
                || light.position.iter().all(|c| c.is_finite());
            if !positive(light.intensity)
                || !placed
                || !light.cone_deg.is_finite()
                || !light.softness.is_finite()
                || !light.range_px.is_finite()
            {
                continue;
            }
            let [r, g, b, _] = crate::color::premultiplied_linear(Color {
                a: 1.0,
                ..light.color
            });
            let radiance = [r, g, b].map(|c| c * light.intensity);
            if light.kind == LightKind::Ambient {
                for (sum, c) in ambient.iter_mut().zip(radiance) {
                    *sum += c;
                }
                continue;
            }
            let aim = view.direction_in_view(light.aim);
            let length = aim.iter().map(|c| c * c).sum::<f64>().sqrt();
            let needs_aim = light.kind != LightKind::Point;
            if needs_aim && !(length > 0.0 && length.is_finite()) {
                continue;
            }
            let aim = if length > 0.0 {
                aim.map(|c| (c / length) as f32)
            } else {
                [0.0, 0.0, -1.0]
            };
            let at = view.in_view(light.position).map(|c| c as f32);
            let kind = match light.kind {
                LightKind::Point => 0.0,
                LightKind::Spot => 1.0,
                _ => 2.0,
            };
            let half = (light.cone_deg.clamp(1.0, 170.0) * 0.5).to_radians();
            let softness = light.softness.clamp(0.0, 1.0);
            let outer = half.cos();
            let inner = (half * (1.0 - softness)).cos();
            sources.push(source);
            packed.push([
                at[0],
                at[1],
                at[2],
                kind,
                aim[0],
                aim[1],
                aim[2],
                light.range_px.max(0.0),
                radiance[0],
                radiance[1],
                radiance[2],
                0.0,
                outer,
                inner,
                0.0,
                0.0,
            ]);
        }
        Self {
            ambient,
            lights: packed,
            sources,
            has_ambient,
        }
    }
}

/// The frame bind group over `frame`, the two shadow maps' views, the environment
/// picture's, `sampler` and the reflection probe's picture's.
fn frame_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    (frame, sampler): (&wgpu::Buffer, &wgpu::Sampler),
    textures: [&wgpu::TextureView; FRAME_TEXTURES],
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("jstream-render-model-frame"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: frame.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(textures[0]),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(textures[1]),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(textures[2]),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(textures[3]),
            },
        ],
    })
}

/// A shadow map's view in the shader's terms (`model.wgsl`'s `Shadow`): the light's
/// position and axes in `view`'s viewing frame, with the packed light it shadows, its
/// focal length, its principal point and its filter radius in map pixels; `None` when
/// its light isn't packed or its camera can't be seen.
pub(crate) fn shadow_view(
    shadow: &ShadowMap<'_>,
    lighting: &SceneLighting,
    view: &jstream_protocol::CameraView,
) -> Option<[f32; 16]> {
    let slot = lighting.sources.iter().position(|&s| s == shadow.light)?;
    let size = (shadow.image.width(), shadow.image.height());
    let seen = shadow
        .camera
        .view(Canvas {
            width: size.0,
            height: size.1,
        })
        .ok()?;
    let at = view.in_view(seen.position()).map(|c| c as f32);
    let [right, down, forward] = seen
        .axes()
        .map(|axis| view.direction_in_view(axis).map(|c| c as f32));
    // The map is square: one principal coordinate serves both axes.
    let [cx, _] = seen.principal();
    // A hard edge still filters over a pixel; the softest over 12.
    let radius = 0.75 + 11.25 * shadow.softness.clamp(0.0, 1.0);
    Some([
        at[0],
        at[1],
        at[2],
        slot as f32,
        right[0],
        right[1],
        right[2],
        seen.focal_px(),
        down[0],
        down[1],
        down[2],
        cx,
        forward[0],
        forward[1],
        forward[2],
        radius,
    ])
}

/// An environment in the shader's terms (`model.wgsl`'s `Environment`): its colours as
/// linear light times its intensity, its turn, how its picture stores alpha (-1 for
/// none), whether the scene has an ambient light, and the camera's axes, which take the
/// viewing frame back to the scene's space.
fn environment_values(
    environment: &Environment<'_>,
    image: Option<&Image>,
    lighting: &SceneLighting,
    view: &jstream_protocol::CameraView,
) -> [f32; 24] {
    let intensity = if environment.intensity.is_finite() {
        environment.intensity.max(0.0)
    } else {
        0.0
    };
    let linear = |color: Color| {
        let [r, g, b, _] = crate::color::premultiplied_linear(Color { a: 1.0, ..color });
        [r, g, b].map(|c| c * intensity)
    };
    let [sky, horizon, ground] =
        [environment.sky, environment.horizon, environment.ground].map(linear);
    let [right, down, forward] = environment.sky_axes(view.axes());
    [
        sky[0],
        sky[1],
        sky[2],
        1.0,
        horizon[0],
        horizon[1],
        horizon[2],
        0.0,
        ground[0],
        ground[1],
        ground[2],
        image.map_or(-1.0, |image| image.alpha().shader_mode()),
        right[0],
        right[1],
        right[2],
        if lighting.has_ambient { 1.0 } else { 0.0 },
        down[0],
        down[1],
        down[2],
        intensity,
        forward[0],
        forward[1],
        forward[2],
        0.0,
    ]
}

/// The environment uniform for a model's `reflection` (ADR-0107 decision 5): no
/// gradient, the picture at intensity 1 and no turn, its panorama about the camera's
/// axes in a 3D scene, about the viewing frame (x right, y down, looking along -z) in a
/// 2D one. Ambient light scales it only for a model lit by a scene that has some.
fn reflection_values(
    image: &Image,
    lighting: Option<&SceneLighting>,
    view: Option<&jstream_protocol::CameraView>,
) -> [f32; 24] {
    let [right, down, forward] = view.map_or(
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
        |view| view.axes().map(|axis| axis.map(|c| c as f32)),
    );
    let ambient = lighting.is_some_and(|l| l.has_ambient);
    [
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        image.alpha().shader_mode(),
        right[0],
        right[1],
        right[2],
        if ambient { 1.0 } else { 0.0 },
        down[0],
        down[1],
        down[2],
        1.0,
        forward[0],
        forward[1],
        forward[2],
        0.0,
    ]
}

/// The frame uniform's values after the lights: the shadow maps' matrices, the
/// environment, the floor cut and the reflection probe.
type FrameTail<'a> = (
    &'a [[f32; 16]],
    Option<&'a [f32; 24]>,
    [f32; 4],
    Option<&'a [f32; 20]>,
);

fn frame_bytes(
    (solid, mirrored): (&Solid, bool),
    clip: &[[f32; 4]; 4],
    lighting: Lighting,
    lights: Option<&SceneLighting>,
    (shadows, environment, cut, probe): FrameTail<'_>,
) -> Vec<u8> {
    let mut values = Vec::with_capacity((FRAME_BYTES / 4) as usize);
    values.extend(clip.iter().flatten());
    values.extend(solid.view_matrix().iter().flatten());
    for column in solid.normal_matrix() {
        values.extend(column);
        values.push(0.0);
    }
    let perspective = solid.perspective_px().unwrap_or(0.0) as f32;
    values.extend([
        perspective,
        if mirrored { 1.0 } else { 0.0 },
        if lighting == Lighting::Unlit {
            1.0
        } else {
            0.0
        },
        if lights.is_some() { 1.0 } else { 0.0 },
    ]);
    let (ambient, count) = lights.map_or(([0.0; 3], 0), |l| (l.ambient, l.lights.len()));
    values.extend(ambient);
    values.push(count as f32);
    for light in lights.into_iter().flat_map(|l| &l.lights) {
        values.extend(light);
    }
    // The shadow maps' views follow every light slot; an unused one shadows no slot.
    values.resize(
        (64 + 64 + 48 + 16 + 16 + MAX_SCENE_LIGHTS as u64 * LIGHT_BYTES) as usize / 4,
        0.0,
    );
    for slot in 0..MAX_SHADOW_MAPS {
        match shadows.get(slot) {
            Some(shadow) => values.extend(shadow),
            None => {
                let mut none = [0.0f32; 16];
                none[3] = -1.0;
                values.extend(none);
            }
        }
    }
    // The environment follows the shadow maps; none leaves the studio gradient.
    values.extend(environment.copied().unwrap_or([0.0; 24]));
    // The floor's cutting plane follows the environment, and the probe follows it.
    values.extend(cut);
    values.extend(probe.copied().unwrap_or([0.0; 20]));
    values.resize((FRAME_BYTES / 4) as usize, 0.0);
    f32_bytes(&values)
}

/// Whether the viewer sees the side of `solid`'s `z = 0` plane its normal points to:
/// the shaded normal's own side.
fn sees_front(solid: &Solid) -> bool {
    let normal = solid.normal_matrix();
    let n = [0, 1, 2].map(|r| f64::from(normal[2][r]));
    let at = solid.in_view([0.0; 3]);
    let to_viewer = match solid.perspective_px() {
        Some(p) => [-at[0], -at[1], p - at[2]],
        None => [0.0, 0.0, 1.0],
    };
    n[0] * to_viewer[0] + n[1] * to_viewer[1] + n[2] * to_viewer[2] > 0.0
}

/// `probe` in the shader's terms (`model.wgsl`'s `Probe`) as `view` sees it: its box's
/// centre in the viewing frame and the scene's axes there, each with the box's half size
/// along it, then its fade and how its picture stores alpha.
fn probe_values(probe: &ProbeLookup<'_>, view: &jstream_protocol::CameraView) -> [f32; 20] {
    let [right, down, forward] = view.axes();
    let focal = f64::from(view.focal_px());
    let position = view.position();
    // The viewing frame's origin: on the camera's axis, its focal length ahead.
    let origin = [0, 1, 2].map(|i| position[i] + forward[i] * focal);
    let off = [0, 1, 2].map(|i| f64::from(probe.centre[i]) - origin[i]);
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let centre = [dot(off, right), dot(off, down), -dot(off, forward)];
    let finite = |value: f32| {
        if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        }
    };
    let axis = |i: usize| [right[i] as f32, down[i] as f32, -forward[i] as f32];
    let [x, y, z] = [axis(0), axis(1), axis(2)];
    let [hx, hy, hz] = probe.half.map(finite);
    [
        centre[0] as f32,
        centre[1] as f32,
        centre[2] as f32,
        1.0,
        x[0],
        x[1],
        x[2],
        hx,
        y[0],
        y[1],
        y[2],
        hy,
        z[0],
        z[1],
        z[2],
        hz,
        finite(probe.fade),
        probe.image.alpha().shader_mode(),
        0.0,
        0.0,
    ]
}

/// How far past its floor (ADR-0151 decision 4) a viewing-frame point `s` is, as the
/// shader reads it: `dot(xyz, s) + w`, at or above 0 at or below `y = floor` of the
/// scene's space, less half a pixel so the floor itself is cut. A camera's frame is
/// `s = (rel · right, rel · down, focal − rel · forward)` of `rel` from its position, so
/// the scene's `y` of `s` is `position.y + right.y s.x + down.y s.y + forward.y (focal −
/// s.z)`. No floor, or no camera, never cuts.
fn floor_cut(floor: Option<f32>, view: Option<&jstream_protocol::CameraView>) -> [f32; 4] {
    let (Some(floor), Some(view)) = (floor.filter(|floor| floor.is_finite()), view) else {
        return [0.0, 0.0, 0.0, -1.0];
    };
    let [right, down, forward] = view.axes();
    let focal = f64::from(view.focal_px());
    let at = view.position()[1] + forward[1] * focal - f64::from(floor) + 0.5;
    [right[1], down[1], -forward[1], at].map(|c| c as f32)
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

const IDENTITY_MAT4: rig::Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// A palette's storage buffer, holding `palette`, and its bind group.
fn pose_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    palette: &[rig::Mat4],
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let bytes: Vec<u8> = palette
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("jstream-render-model-pose"),
        contents: &bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("jstream-render-model-pose"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    (buffer, bind)
}

/// Uploads `texture` with its mips, and says in what format: block compressed when `compress` and its sides are
/// whole blocks (`bc.rs`, BC1 when every texel is opaque, else BC3), else as RGBA8.
fn upload_texture(
    kit: &UploadKit,
    texture: &ModelTexture,
    compress: bool,
) -> Result<(wgpu::TextureView, wgpu::TextureFormat), RenderError> {
    let limit = kit.max_texture_dimension_2d;
    if texture.width() > limit || texture.height() > limit {
        return Err(RenderError::TooLarge {
            what: "image",
            width: texture.width(),
            height: texture.height(),
            limit,
        });
    }
    let srgb = texture.color() == TextureColor::Srgb;
    // wgpu takes a compressed texture only in whole blocks at level 0.
    let compress =
        compress && texture.width().is_multiple_of(4) && texture.height().is_multiple_of(4);
    let alpha = compress && !texture.levels().iter().all(|level| bc::opaque(level));
    let (format, block) = match (compress, alpha, srgb) {
        (false, _, true) => (wgpu::TextureFormat::Rgba8UnormSrgb, None),
        (false, _, false) => (wgpu::TextureFormat::Rgba8Unorm, None),
        (true, false, true) => (wgpu::TextureFormat::Bc1RgbaUnormSrgb, Some(bc::BC1_BLOCK)),
        (true, false, false) => (wgpu::TextureFormat::Bc1RgbaUnorm, Some(bc::BC1_BLOCK)),
        (true, true, true) => (wgpu::TextureFormat::Bc3RgbaUnormSrgb, Some(bc::BC3_BLOCK)),
        (true, true, false) => (wgpu::TextureFormat::Bc3RgbaUnorm, Some(bc::BC3_BLOCK)),
    };
    let uploaded = kit.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jstream-render-model-texture"),
        size: wgpu::Extent3d {
            width: texture.width(),
            height: texture.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: texture.levels().len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Each level is written from where it is, a band of rows at a time: the queue is
    // locked for one band's copy, not a level's, so a thread that submits meanwhile
    // waits for little (ADR-0177). A compressed level's rows are rows of blocks, each
    // four texels high.
    for (level, texels) in texture.levels().iter().enumerate() {
        let width = (texture.width() >> level).max(1);
        let height = (texture.height() >> level).max(1);
        let (bytes, row, texel_rows, copy_width) = match block {
            None => (
                std::borrow::Cow::Borrowed(texels.as_slice()),
                width as usize * 4,
                1,
                width,
            ),
            Some(size) => (
                std::borrow::Cow::Owned(bc::compress(texels, width, height, alpha)),
                width.div_ceil(4) as usize * size,
                4,
                width.div_ceil(4) * 4,
            ),
        };
        let band = (TEXTURE_BAND_BYTES / row).max(1);
        for (at, rows) in bytes.chunks(row * band).enumerate() {
            let rows_here = (rows.len() / row) as u32;
            kit.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &uploaded,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: (at * band) as u32 * texel_rows,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                rows,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row as u32),
                    rows_per_image: Some(rows_here),
                },
                wgpu::Extent3d {
                    width: copy_width,
                    height: rows_here * texel_rows,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
    Ok((
        uploaded.create_view(&wgpu::TextureViewDescriptor::default()),
        format,
    ))
}

mod bc;
mod depth_resolve;
pub(crate) mod focus;
mod msaa;

#[cfg(test)]
mod tests;
