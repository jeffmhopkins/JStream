//! 3D models on the CPU (ADR-0107): triangle meshes, metallic-roughness materials and
//! their textures, in the scene's axes, ready for a GPU upload.
//!
//! A [`Model`] is built off the render thread (a loader or [`shapes`]) and checked once:
//! its indices are in range, its materials and textures exist, and it fits the caps of
//! ADR-0107 decision 2. Its axes are a scene's: x right, y down, z toward the viewer.
//! A front face's vertices `a`, `b`, `c` wind so that `(b − a) × (c − a)` points out of
//! it, which is clockwise on the y-down canvas when it faces the viewer.

pub mod shapes;

use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

/// The most triangles a model may have (ADR-0107 decision 2).
pub const MAX_TRIANGLES: usize = 2_000_000;
/// The most textures a model may have.
pub const MAX_TEXTURES: usize = 64;
/// The longest side a model's texture may have, in texels; a loader scales a larger one
/// down.
pub const MAX_TEXTURE_SIDE: u32 = 4096;
/// The most memory a model may take on the GPU: its vertex and index data and its
/// textures with their mips.
pub const MAX_MODEL_BYTES: u64 = 512 * 1024 * 1024;

/// Why a model can't be built.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// A triangle names a vertex the primitive doesn't have.
    #[error("primitive {primitive} names vertex {index} of {vertices}")]
    Index {
        /// The primitive.
        primitive: usize,
        /// The index.
        index: u32,
        /// How many vertices the primitive has.
        vertices: usize,
    },
    /// The index count is not a multiple of three.
    #[error("primitive {primitive} has {indices} indices, not whole triangles")]
    Triangles {
        /// The primitive.
        primitive: usize,
        /// How many indices it has.
        indices: usize,
    },
    /// A primitive names a material the model doesn't have.
    #[error("primitive {primitive} names material {material} of {materials}")]
    Material {
        /// The primitive.
        primitive: usize,
        /// The material.
        material: usize,
        /// How many materials the model has.
        materials: usize,
    },
    /// A material names a texture the model doesn't have.
    #[error("material {material} names texture {texture} of {textures}")]
    Texture {
        /// The material.
        material: usize,
        /// The texture.
        texture: usize,
        /// How many textures the model has.
        textures: usize,
    },
    /// A texture's bytes don't match its size, or it is empty or too large.
    #[error("texture {texture} is {width}x{height} with {bytes} bytes")]
    TextureBytes {
        /// The texture.
        texture: usize,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
        /// How many bytes it has.
        bytes: usize,
    },
    /// A vertex position is not finite.
    #[error("primitive {primitive} has a vertex that is not finite")]
    NotFinite {
        /// The primitive.
        primitive: usize,
    },
    /// The model is past one of ADR-0107's caps.
    #[error("the model has {got} {what}, more than the {limit} a model may have")]
    TooLarge {
        /// What is counted.
        what: &'static str,
        /// How many it has.
        got: u64,
        /// The cap.
        limit: u64,
    },
    /// The model has no triangles.
    #[error("the model has no triangles")]
    Empty,
}

/// One vertex of a mesh, in the model's own space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelVertex {
    /// Position: x right, y down, z toward the viewer.
    pub position: [f32; 3],
    /// Unit normal, pointing out of the front face.
    pub normal: [f32; 3],
    /// Unit tangent along increasing u, with the bitangent's sign in `w` (glTF's
    /// convention), for normal maps.
    pub tangent: [f32; 4],
    /// Texture coordinate: (0, 0) the top-left of a texture, v down.
    pub uv: [f32; 2],
    /// Vertex colour, linear and straight, multiplying the base colour.
    pub color: [f32; 4],
}

impl ModelVertex {
    /// A white vertex at `position` with `normal` and `uv`, and a tangent along x.
    pub fn new(position: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Self {
        Self {
            position,
            normal,
            tangent: [1.0, 0.0, 0.0, 1.0],
            uv,
            color: [1.0; 4],
        }
    }
}

/// How a material's alpha is used (glTF's `alphaMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AlphaMode {
    /// Alpha is ignored; the surface is opaque.
    #[default]
    Opaque,
    /// A fragment under the cutoff is not drawn; the rest is opaque.
    Mask {
        /// The cutoff, glTF's `alphaCutoff` (0.5 by default).
        cutoff: f32,
    },
    /// Alpha blends the surface over what is behind it.
    Blend,
}

/// A texture a material reads, and which texture coordinates it reads with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureRef {
    /// The texture's index in [`Model::textures`].
    pub texture: usize,
}

/// A metallic-roughness material (glTF 2.0's core material).
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    /// Its name in the file, or the shape's (`"shape"`).
    pub name: String,
    /// Base colour, linear and straight.
    pub base_color: [f32; 4],
    /// The base colour texture (sRGB), multiplied by `base_color`.
    pub base_color_texture: Option<TextureRef>,
    /// Metalness, 0 to 1.
    pub metallic: f32,
    /// Roughness, 0 to 1.
    pub roughness: f32,
    /// Roughness in green and metalness in blue (linear), multiplying the factors.
    pub metallic_roughness_texture: Option<TextureRef>,
    /// A tangent-space normal map (linear).
    pub normal_texture: Option<TextureRef>,
    /// How much the normal map bends the normal.
    pub normal_scale: f32,
    /// Ambient occlusion in red (linear).
    pub occlusion_texture: Option<TextureRef>,
    /// How much the occlusion texture darkens.
    pub occlusion_strength: f32,
    /// Emitted light, linear; may be above 1.
    pub emissive: [f32; 3],
    /// The emissive texture (sRGB), multiplied by `emissive`.
    pub emissive_texture: Option<TextureRef>,
    /// How alpha is used.
    pub alpha_mode: AlphaMode,
    /// Whether back faces are drawn too, lit from their own side.
    pub double_sided: bool,
    /// Whether it shows base colour and emission only, with no lighting.
    pub unlit: bool,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: [1.0; 4],
            base_color_texture: None,
            metallic: 0.0,
            roughness: 0.5,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive: [0.0; 3],
            emissive_texture: None,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
        }
    }
}

/// How a model is lit (ADR-0107 decision 4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lighting {
    /// The studio rig, fixed to the camera: a key light above and to the left, a fill
    /// opposite it, and a soft environment.
    #[default]
    Studio,
    /// Base colour and emission as authored.
    Unlit,
}

impl Lighting {
    /// Its name as a setting spells it.
    pub fn name(self) -> &'static str {
        match self {
            Lighting::Studio => "studio",
            Lighting::Unlit => "unlit",
        }
    }

    /// The lighting a setting names.
    pub fn from_name(name: &str) -> Option<Self> {
        [Lighting::Studio, Lighting::Unlit]
            .into_iter()
            .find(|lighting| lighting.name() == name)
    }
}

/// Whether a texture holds colour (sRGB-encoded) or data (linear).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureColor {
    /// sRGB colour: base colour and emissive textures.
    Srgb,
    /// Linear data: normals, metalness and roughness, occlusion.
    Linear,
}

/// A texture with its mips, straight RGBA8, built off the render thread.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelTexture {
    width: u32,
    height: u32,
    color: TextureColor,
    /// Level 0 first, each half the size of the one before, down to 1x1.
    levels: Vec<Vec<u8>>,
}

impl ModelTexture {
    /// A texture of `width` by `height` straight RGBA8, with its mips built here: each
    /// level a 2×2 box filter of the one above (on an odd side the last row or column
    /// is left out), in linear light for an sRGB texture.
    ///
    /// # Errors
    /// [`ModelError::TextureBytes`] (with texture 0; [`Model::new`] names the real index)
    /// when `rgba` is not `width * height * 4` bytes, or a side is 0 or past
    /// [`MAX_TEXTURE_SIDE`].
    pub fn new(
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        color: TextureColor,
    ) -> Result<Self, ModelError> {
        let expected = u64::from(width) * u64::from(height) * 4;
        if width == 0
            || height == 0
            || width > MAX_TEXTURE_SIDE
            || height > MAX_TEXTURE_SIDE
            || rgba.len() as u64 != expected
        {
            return Err(ModelError::TextureBytes {
                texture: 0,
                width,
                height,
                bytes: rgba.len(),
            });
        }
        let mut levels = vec![rgba];
        let (mut w, mut h) = (width, height);
        while w > 1 || h > 1 {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let above = levels.last().map(Vec::as_slice).unwrap_or_default();
            levels.push(downsample(above, w, h, nw, nh, color));
            (w, h) = (nw, nh);
        }
        Ok(Self {
            width,
            height,
            color,
            levels,
        })
    }

    /// Width of level 0.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height of level 0.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Whether it is colour or data.
    pub fn color(&self) -> TextureColor {
        self.color
    }

    /// Its levels, level 0 first, straight RGBA8 with no row padding.
    pub fn levels(&self) -> &[Vec<u8>] {
        &self.levels
    }

    fn bytes(&self) -> u64 {
        self.levels.iter().map(|level| level.len() as u64).sum()
    }
}

fn downsample(above: &[u8], w: u32, h: u32, nw: u32, nh: u32, color: TextureColor) -> Vec<u8> {
    let decode = |v: u8| -> f32 {
        let c = f32::from(v) / 255.0;
        match color {
            TextureColor::Linear => c,
            TextureColor::Srgb if c <= 0.04045 => c / 12.92,
            TextureColor::Srgb => ((c + 0.055) / 1.055).powf(2.4),
        }
    };
    let encode = |c: f32| -> u8 {
        let c = c.clamp(0.0, 1.0);
        let v = match color {
            TextureColor::Linear => c,
            TextureColor::Srgb if c <= 0.003_130_8 => c * 12.92,
            TextureColor::Srgb => 1.055 * c.powf(1.0 / 2.4) - 0.055,
        };
        (v * 255.0 + 0.5) as u8
    };
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0.0f32; 4];
            let mut count = 0.0;
            for sy in (y * 2)..((y * 2 + 2).min(h)) {
                for sx in (x * 2)..((x * 2 + 2).min(w)) {
                    let at = ((sy * w + sx) * 4) as usize;
                    let a = f32::from(above[at + 3]) / 255.0;
                    // Weighted by alpha, so a transparent texel's colour doesn't bleed.
                    for c in 0..3 {
                        sum[c] += decode(above[at + c]) * a;
                    }
                    sum[3] += a;
                    count += 1.0;
                }
            }
            let alpha = sum[3] / count;
            for c in 0..3 {
                let value = if sum[3] > 0.0 { sum[c] / sum[3] } else { 0.0 };
                out.push(encode(value));
            }
            out.push((alpha * 255.0 + 0.5) as u8);
        }
    }
    out
}

/// One mesh with one material.
#[derive(Clone, Debug, PartialEq)]
pub struct Primitive {
    /// Its vertices.
    pub vertices: Vec<ModelVertex>,
    /// Three indices per triangle into `vertices`.
    pub indices: Vec<u32>,
    /// Its material's index in [`Model::materials`].
    pub material: usize,
}

/// The box around a model, in its own space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// The smallest x, y and z.
    pub min: [f32; 3],
    /// The largest x, y and z.
    pub max: [f32; 3],
}

impl Bounds {
    /// The centre.
    pub fn center(&self) -> [f32; 3] {
        [0, 1, 2].map(|i| (self.min[i] + self.max[i]) * 0.5)
    }

    /// Width, height and depth.
    pub fn size(&self) -> [f32; 3] {
        [0, 1, 2].map(|i| self.max[i] - self.min[i])
    }
}

static NEXT_VERSION: AtomicU64 = AtomicU64::new(1);

/// A checked model: its primitives, materials and textures, its bounds, and a version
/// that is new for every model built, so a GPU upload can be cached by it.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    primitives: Vec<Primitive>,
    materials: Vec<Material>,
    textures: Vec<ModelTexture>,
    bounds: Bounds,
    version: u64,
}

impl Model {
    /// Checks and builds a model.
    ///
    /// # Errors
    /// A [`ModelError`] naming the first problem: an index, material or texture out of
    /// range, a vertex that isn't finite, no triangles at all, or a cap of ADR-0107.
    pub fn new(
        primitives: Vec<Primitive>,
        materials: Vec<Material>,
        textures: Vec<ModelTexture>,
    ) -> Result<Self, ModelError> {
        if textures.len() > MAX_TEXTURES {
            return Err(too_large(
                "textures",
                textures.len() as u64,
                MAX_TEXTURES as u64,
            ));
        }
        for (material, m) in materials.iter().enumerate() {
            let refs = [
                m.base_color_texture,
                m.metallic_roughness_texture,
                m.normal_texture,
                m.occlusion_texture,
                m.emissive_texture,
            ];
            for texture in refs.into_iter().flatten() {
                if texture.texture >= textures.len() {
                    return Err(ModelError::Texture {
                        material,
                        texture: texture.texture,
                        textures: textures.len(),
                    });
                }
            }
        }
        let mut triangles = 0u64;
        let mut bytes: u64 = textures.iter().map(ModelTexture::bytes).sum();
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for (primitive, p) in primitives.iter().enumerate() {
            if p.material >= materials.len() {
                return Err(ModelError::Material {
                    primitive,
                    material: p.material,
                    materials: materials.len(),
                });
            }
            if p.indices.len() % 3 != 0 {
                return Err(ModelError::Triangles {
                    primitive,
                    indices: p.indices.len(),
                });
            }
            if let Some(&index) = p.indices.iter().find(|&&i| i as usize >= p.vertices.len()) {
                return Err(ModelError::Index {
                    primitive,
                    index,
                    vertices: p.vertices.len(),
                });
            }
            for v in &p.vertices {
                if !v.position.iter().all(|c| c.is_finite()) {
                    return Err(ModelError::NotFinite { primitive });
                }
            }
            // Only vertices that a triangle uses count toward the bounds.
            for &i in &p.indices {
                let at = p.vertices[i as usize].position;
                for c in 0..3 {
                    min[c] = min[c].min(at[c]);
                    max[c] = max[c].max(at[c]);
                }
            }
            triangles += p.indices.len() as u64 / 3;
            bytes += p.vertices.len() as u64 * VERTEX_BYTES + p.indices.len() as u64 * 4;
        }
        if triangles == 0 {
            return Err(ModelError::Empty);
        }
        if triangles > MAX_TRIANGLES as u64 {
            return Err(too_large("triangles", triangles, MAX_TRIANGLES as u64));
        }
        if bytes > MAX_MODEL_BYTES {
            return Err(too_large("bytes of GPU memory", bytes, MAX_MODEL_BYTES));
        }
        Ok(Self {
            primitives,
            materials,
            textures,
            bounds: Bounds { min, max },
            version: NEXT_VERSION.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// Its primitives.
    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    /// Its materials.
    pub fn materials(&self) -> &[Material] {
        &self.materials
    }

    /// Its textures.
    pub fn textures(&self) -> &[ModelTexture] {
        &self.textures
    }

    /// The box around the vertices its triangles use.
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// New for every model built: what a GPU upload is cached by.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The same model with its materials changed by `change`, under a new version: how a
    /// live material setting (a colour, `unlit`) applies without reloading the mesh.
    pub fn with_materials(&self, change: impl Fn(&mut Material)) -> Self {
        let mut out = self.clone();
        for material in &mut out.materials {
            change(material);
        }
        out.version = NEXT_VERSION.fetch_add(1, Ordering::Relaxed);
        out
    }

    /// Triangles in all.
    pub fn triangles(&self) -> usize {
        self.primitives.iter().map(|p| p.indices.len() / 3).sum()
    }
}

/// Fills in each vertex's tangent from the texture coordinates of the triangles that use
/// it (glTF's rule for a mesh that has none): along increasing u, made perpendicular to
/// the normal, with the sign of the bitangent along increasing v in `w`. A vertex whose
/// triangles have no usable texture coordinates gets any tangent perpendicular to its
/// normal.
pub fn compute_tangents(primitive: &mut Primitive) {
    let n = primitive.vertices.len();
    let mut tangents = vec![[0.0f32; 3]; n];
    let mut bitangents = vec![[0.0f32; 3]; n];
    for t in primitive.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| primitive.vertices[i as usize]);
        let e1 = [0, 1, 2].map(|i| b.position[i] - a.position[i]);
        let e2 = [0, 1, 2].map(|i| c.position[i] - a.position[i]);
        let (du1, dv1) = (b.uv[0] - a.uv[0], b.uv[1] - a.uv[1]);
        let (du2, dv2) = (c.uv[0] - a.uv[0], c.uv[1] - a.uv[1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-12 {
            continue;
        }
        let r = 1.0 / det;
        let tangent = [0, 1, 2].map(|i| (e1[i] * dv2 - e2[i] * dv1) * r);
        let bitangent = [0, 1, 2].map(|i| (e2[i] * du1 - e1[i] * du2) * r);
        for &i in t {
            for c in 0..3 {
                tangents[i as usize][c] += tangent[c];
                bitangents[i as usize][c] += bitangent[c];
            }
        }
    }
    for (i, vertex) in primitive.vertices.iter_mut().enumerate() {
        let normal = vertex.normal;
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let mut t = tangents[i];
        let along = dot(t, normal);
        t = [0, 1, 2].map(|c| t[c] - normal[c] * along);
        let mut length = dot(t, t).sqrt();
        if length < 1e-8 {
            // Any direction perpendicular to the normal.
            let other = if normal[0].abs() < 0.9 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            };
            let along = dot(other, normal);
            t = [0, 1, 2].map(|c| other[c] - normal[c] * along);
            length = dot(t, t).sqrt().max(1e-8);
        }
        let t = t.map(|c| c / length);
        let cross = [
            normal[1] * t[2] - normal[2] * t[1],
            normal[2] * t[0] - normal[0] * t[2],
            normal[0] * t[1] - normal[1] * t[0],
        ];
        let sign = if dot(cross, bitangents[i]) < 0.0 {
            -1.0
        } else {
            1.0
        };
        vertex.tangent = [t[0], t[1], t[2], sign];
    }
}

/// The bytes one vertex takes on the GPU: 16 `f32`s.
pub const VERTEX_BYTES: u64 = 64;

fn too_large(what: &'static str, got: u64, limit: u64) -> ModelError {
    ModelError::TooLarge { what, got, limit }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle(material: usize) -> Primitive {
        let v = |x, y| ModelVertex::new([x, y, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        Primitive {
            vertices: vec![v(0.0, 0.0), v(1.0, 0.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2],
            material,
        }
    }

    #[test]
    fn a_model_knows_its_bounds_and_gets_a_new_version() {
        let a = Model::new(vec![triangle(0)], vec![Material::default()], vec![]).unwrap();
        assert_eq!(a.bounds().min, [0.0, 0.0, 0.0]);
        assert_eq!(a.bounds().max, [1.0, 1.0, 0.0]);
        assert_eq!(a.triangles(), 1);
        let b = a.with_materials(|m| m.unlit = true);
        assert_ne!(a.version(), b.version());
        assert!(b.materials()[0].unlit);
    }

    #[test]
    fn out_of_range_references_are_refused() {
        let mut bad = triangle(0);
        bad.indices = vec![0, 1, 3];
        assert!(matches!(
            Model::new(vec![bad], vec![Material::default()], vec![]),
            Err(ModelError::Index { index: 3, .. })
        ));
        assert!(matches!(
            Model::new(vec![triangle(1)], vec![Material::default()], vec![]),
            Err(ModelError::Material { material: 1, .. })
        ));
        let textured = Material {
            base_color_texture: Some(TextureRef { texture: 0 }),
            ..Material::default()
        };
        assert!(matches!(
            Model::new(vec![triangle(0)], vec![textured], vec![]),
            Err(ModelError::Texture { texture: 0, .. })
        ));
        assert_eq!(
            Model::new(vec![], vec![Material::default()], vec![]),
            Err(ModelError::Empty)
        );
        let mut nan = triangle(0);
        nan.vertices[0].position[0] = f32::NAN;
        assert!(matches!(
            Model::new(vec![nan], vec![Material::default()], vec![]),
            Err(ModelError::NotFinite { .. })
        ));
    }

    #[test]
    fn textures_build_their_mips_down_to_one_texel() {
        let texture = ModelTexture::new(4, 2, vec![255; 32], TextureColor::Srgb).unwrap();
        let sizes: Vec<usize> = texture.levels().iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![32, 8, 4]);
        assert!(ModelTexture::new(2, 2, vec![0; 3], TextureColor::Linear).is_err());
        assert!(
            ModelTexture::new(
                MAX_TEXTURE_SIDE + 1,
                1,
                vec![0; 4 * 4097],
                TextureColor::Linear
            )
            .is_err()
        );
    }

    #[test]
    fn a_mip_of_srgb_averages_in_linear_light_and_ignores_transparent_colour() {
        // Black and white average to linear 0.5, which is sRGB 188, not 128.
        let rgba = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let texture = ModelTexture::new(2, 1, rgba, TextureColor::Srgb).unwrap();
        assert_eq!(texture.levels()[1], vec![188, 188, 188, 255]);
        // A transparent red texel lends no red.
        let rgba = vec![255, 0, 0, 0, 0, 0, 255, 255];
        let texture = ModelTexture::new(2, 1, rgba, TextureColor::Linear).unwrap();
        assert_eq!(texture.levels()[1], vec![0, 0, 255, 128]);
    }
}
