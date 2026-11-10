//! A glTF 2.0 loader for the `model` source (ADR-0107 decision 2), on `serde_json`, so
//! it adds no dependency.
//!
//! It reads a `.glb` (its JSON chunk and binary chunk) or a `.gltf` whose buffers and
//! images are `data:` URIs or files beside it in the same asset root, and builds a static
//! [`Model`] of the default scene: every mesh instance with its node's world transform
//! baked in, in the scene's axes (glTF's y up becomes y down, and each triangle's winding
//! is swapped to match). Skins, morph targets, animations, cameras and lights are ignored
//! (each kind logged once); a required extension the loader lacks is an error naming it.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use jstream_media::model::{
    AlphaMode, MAX_MODEL_BYTES, MAX_TEXTURE_SIDE, MAX_TEXTURES, MAX_TRIANGLES, Material, Model,
    ModelTexture, ModelVertex, Primitive, TextureColor, TextureRef, VERTEX_BYTES, compute_tangents,
};
use jstream_media::model::rig::{
    self, Channel, Clip, Interpolation, Path, Rest, Rig, RigNode, Slot,
};
use jstream_protocol::{AssetRoot, check_asset_path};
use serde::Deserialize;

/// Extensions the loader understands. Any other the file *requires* is refused.
const SUPPORTED: [&str; 2] = ["KHR_materials_unlit", "KHR_materials_emissive_strength"];

const GLB_MAGIC: u32 = 0x4654_6C67;
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;

/// Why a file isn't a model the loader can build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadError(pub(crate) String);

impl LoadError {
    fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

type Result<T> = std::result::Result<T, LoadError>;

/// How the loader gets the files a `.gltf` names: a relative URI, resolved against the
/// file's own folder, read through the asset roots by the caller.
pub(crate) trait Resources {
    /// The bytes of the file at `uri`, relative to the model file.
    fn read(&mut self, uri: &str) -> Result<Vec<u8>>;
    /// Decodes a PNG or JPEG into straight RGBA8: width, height and pixels.
    fn decode(&mut self, bytes: &[u8], name: &str) -> Result<(u32, u32, Vec<u8>)>;
}

/// What the loader ignored, so the caller can log each once.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Ignored(pub(crate) BTreeSet<&'static str>);

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Document {
    #[serde(default)]
    extensions_used: Vec<String>,
    #[serde(default)]
    extensions_required: Vec<String>,
    #[serde(default)]
    scene: Option<usize>,
    #[serde(default)]
    scenes: Vec<SceneDef>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    meshes: Vec<Mesh>,
    #[serde(default)]
    accessors: Vec<Accessor>,
    #[serde(default)]
    buffer_views: Vec<BufferView>,
    #[serde(default)]
    buffers: Vec<Buffer>,
    #[serde(default)]
    materials: Vec<MaterialDef>,
    #[serde(default)]
    textures: Vec<Texture>,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    skins: Vec<SkinDef>,
    #[serde(default)]
    animations: Vec<AnimationDef>,
    #[serde(default)]
    cameras: Vec<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct SceneDef {
    #[serde(default)]
    nodes: Vec<usize>,
}

#[derive(Deserialize, Default)]
struct Node {
    #[serde(default)]
    children: Vec<usize>,
    #[serde(default)]
    mesh: Option<usize>,
    #[serde(default)]
    matrix: Option<[f64; 16]>,
    #[serde(default)]
    translation: Option<[f64; 3]>,
    #[serde(default)]
    rotation: Option<[f64; 4]>,
    #[serde(default)]
    scale: Option<[f64; 3]>,
    #[serde(default)]
    weights: Option<Vec<f64>>,
    #[serde(default)]
    skin: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SkinDef {
    #[serde(default)]
    joints: Vec<usize>,
    #[serde(default)]
    inverse_bind_matrices: Option<usize>,
}

#[derive(Deserialize, Default)]
struct AnimationDef {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    channels: Vec<ChannelDef>,
    #[serde(default)]
    samplers: Vec<SamplerDef>,
}

#[derive(Deserialize, Default)]
struct ChannelDef {
    sampler: usize,
    target: TargetDef,
}

#[derive(Deserialize, Default)]
struct TargetDef {
    #[serde(default)]
    node: Option<usize>,
    path: String,
}

#[derive(Deserialize, Default)]
struct SamplerDef {
    input: usize,
    output: usize,
    #[serde(default)]
    interpolation: Option<String>,
}

#[derive(Deserialize, Default)]
struct Mesh {
    #[serde(default)]
    primitives: Vec<PrimitiveDef>,
}

#[derive(Deserialize, Default)]
struct PrimitiveDef {
    attributes: BTreeMap<String, usize>,
    #[serde(default)]
    indices: Option<usize>,
    #[serde(default)]
    material: Option<usize>,
    #[serde(default = "triangles")]
    mode: u32,
    #[serde(default)]
    targets: Option<Vec<serde_json::Value>>,
}

fn triangles() -> u32 {
    4
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Accessor {
    #[serde(default)]
    buffer_view: Option<usize>,
    #[serde(default)]
    byte_offset: usize,
    component_type: u32,
    #[serde(default)]
    normalized: bool,
    count: usize,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    sparse: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BufferView {
    buffer: usize,
    #[serde(default)]
    byte_offset: usize,
    byte_length: usize,
    #[serde(default)]
    byte_stride: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Buffer {
    #[serde(default)]
    uri: Option<String>,
    byte_length: usize,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TextureInfo {
    index: usize,
    #[serde(default)]
    tex_coord: usize,
    #[serde(default)]
    scale: Option<f32>,
    #[serde(default)]
    strength: Option<f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pbr {
    #[serde(default = "white")]
    base_color_factor: [f32; 4],
    #[serde(default)]
    base_color_texture: Option<TextureInfo>,
    #[serde(default = "one")]
    metallic_factor: f32,
    #[serde(default = "one")]
    roughness_factor: f32,
    #[serde(default)]
    metallic_roughness_texture: Option<TextureInfo>,
}

impl Default for Pbr {
    fn default() -> Self {
        Self {
            base_color_factor: white(),
            base_color_texture: None,
            metallic_factor: 1.0,
            roughness_factor: 1.0,
            metallic_roughness_texture: None,
        }
    }
}

fn white() -> [f32; 4] {
    [1.0; 4]
}

fn one() -> f32 {
    1.0
}

fn half() -> f32 {
    0.5
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaterialDef {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pbr_metallic_roughness: Pbr,
    #[serde(default)]
    normal_texture: Option<TextureInfo>,
    #[serde(default)]
    occlusion_texture: Option<TextureInfo>,
    #[serde(default)]
    emissive_texture: Option<TextureInfo>,
    #[serde(default)]
    emissive_factor: [f32; 3],
    #[serde(default)]
    alpha_mode: Option<String>,
    #[serde(default = "half")]
    alpha_cutoff: f32,
    #[serde(default)]
    double_sided: bool,
    #[serde(default)]
    extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct Texture {
    #[serde(default)]
    source: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Image {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    buffer_view: Option<usize>,
}

/// A `.glb`'s JSON chunk and its binary chunk, if it has one.
type Chunks<'a> = (&'a [u8], Option<&'a [u8]>);

/// Splits a `.glb` into its JSON and binary chunks, or returns `None` for a file that
/// isn't one (so it is read as `.gltf` JSON).
fn split_glb(bytes: &[u8]) -> Result<Option<Chunks<'_>>> {
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if word(0) != Some(GLB_MAGIC) {
        return Ok(None);
    }
    if word(4) != Some(2) {
        return Err(LoadError::new("only glTF 2.0 binary files are read"));
    }
    let total = (word(8).unwrap_or(0) as usize).min(bytes.len());
    let mut at = 12;
    let (mut json, mut bin) = (None, None);
    while at + 8 <= total {
        let (Some(length), Some(kind)) = (word(at), word(at + 4)) else {
            break;
        };
        let start = at + 8;
        let end = start
            .checked_add(length as usize)
            .filter(|&end| end <= total)
            .ok_or_else(|| LoadError::new("a chunk runs past the end of the file"))?;
        match kind {
            CHUNK_JSON if json.is_none() => json = Some(&bytes[start..end]),
            CHUNK_BIN if bin.is_none() => bin = Some(&bytes[start..end]),
            _ => {}
        }
        at = end;
    }
    let json = json.ok_or_else(|| LoadError::new("the file has no JSON chunk"))?;
    Ok(Some((json, bin)))
}

/// Builds a model from the bytes of a `.glb` or `.gltf` file.
pub(crate) fn load(bytes: &[u8], resources: &mut dyn Resources) -> Result<(Model, Ignored)> {
    let (json, bin) = match split_glb(bytes)? {
        Some((json, bin)) => (json, bin),
        None => (bytes, None),
    };
    let doc: Document = serde_json::from_slice(json)
        .map_err(|err| LoadError::new(format!("the glTF JSON is not valid: {err}")))?;
    if let Some(missing) = doc
        .extensions_required
        .iter()
        .find(|ext| !SUPPORTED.contains(&ext.as_str()))
    {
        return Err(LoadError::new(format!(
            "the file requires the {missing} extension, which JStream can't read"
        )));
    }
    let mut ignored = Ignored::default();
    if !doc.cameras.is_empty() {
        ignored.0.insert("cameras");
    }
    if doc
        .extensions_used
        .iter()
        .any(|ext| !SUPPORTED.contains(&ext.as_str()))
    {
        ignored.0.insert("extensions");
    }
    let buffers = load_buffers(&doc, bin, resources)?;
    let roots = roots(&doc)?;
    // A file with skins or animations is rigged (ADR-0151 decision 7), when its scene
    // is a tree small enough for a rig.
    let rig = if doc.skins.is_empty() && doc.animations.is_empty() {
        None
    } else {
        let tree = tree(&doc, &roots);
        if tree.is_none() {
            ignored.0.insert("skins and animations of a scene too large or not a tree");
        }
        tree
    };
    let mut builder = Builder {
        doc: &doc,
        buffers: &buffers,
        resources,
        textures: Vec::new(),
        texture_of: BTreeMap::new(),
        materials: BTreeMap::new(),
        material_list: Vec::new(),
        primitives: Vec::new(),
        ignored: &mut ignored,
        triangles: 0,
        bytes: 0,
        visits: 0,
        rig,
    };
    for root in roots {
        builder.node(root, IDENTITY, 0)?;
    }
    let rig = match builder.rig.take() {
        Some(tree) => Some(Arc::new(builder.finish_rig(tree)?)),
        None => None,
    };
    let Builder {
        textures,
        material_list,
        primitives,
        ..
    } = builder;
    let model = Model::rigged(primitives, material_list, textures, rig)
        .map_err(|err| LoadError::new(err.to_string()))?;
    Ok((model, ignored))
}

/// The default scene's root nodes.
fn roots(doc: &Document) -> Result<Vec<usize>> {
    Ok(
        match doc
            .scene
            .or(if doc.scenes.is_empty() { None } else { Some(0) })
        {
            Some(scene) => doc
                .scenes
                .get(scene)
                .ok_or_else(|| LoadError::new(format!("the default scene {scene} doesn't exist")))?
                .nodes
                .clone(),
            // No scene: every node no other node names as a child.
            None => {
                let children: BTreeSet<usize> =
                    doc.nodes.iter().flat_map(|n| n.children.clone()).collect();
                (0..doc.nodes.len())
                    .filter(|i| !children.contains(i))
                    .collect()
            }
        },
    )
}

/// A rig being built: the scene's nodes, parents first, and the palette slots handed
/// out so far.
struct Tree {
    nodes: Vec<RigNode>,
    /// Each glTF node's place in `nodes`, for those in the scene.
    rig_of: Vec<Option<usize>>,
    slots: Vec<Slot>,
    /// The slot of each mesh node that isn't skinned, by its glTF index.
    node_slot: BTreeMap<usize, u16>,
    /// The first slot of each skin's joints, by the skin's index.
    skin_base: BTreeMap<usize, u16>,
}

/// The scene's nodes as a rig's tree, parents first: `None` when a node is reached
/// twice (the scene isn't a tree), it holds a node that doesn't exist, or it has more
/// nodes than a rig may.
fn tree(doc: &Document, roots: &[usize]) -> Option<Tree> {
    let mut nodes = Vec::new();
    let mut rig_of = vec![None; doc.nodes.len()];
    let mut stack: Vec<(usize, Option<usize>, usize)> =
        roots.iter().rev().map(|&r| (r, None, 0)).collect();
    while let Some((index, parent, depth)) = stack.pop() {
        let node = doc.nodes.get(index)?;
        if rig_of[index].is_some() || depth > MAX_DEPTH || nodes.len() >= rig::MAX_NODES {
            return None;
        }
        let here = nodes.len();
        rig_of[index] = Some(here);
        let rest = match node.matrix {
            Some(m) => Rest::Matrix(m),
            None => Rest::Trs {
                translation: node.translation.unwrap_or([0.0; 3]),
                rotation: node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]),
                scale: node.scale.unwrap_or([1.0; 3]),
            },
        };
        nodes.push(RigNode { parent, rest });
        for &child in node.children.iter().rev() {
            stack.push((child, Some(here), depth + 1));
        }
    }
    Some(Tree {
        nodes,
        rig_of,
        slots: Vec::new(),
        node_slot: BTreeMap::new(),
        skin_base: BTreeMap::new(),
    })
}

/// Where a mesh's vertices are kept.
#[derive(Clone, Copy)]
enum Placed {
    /// In the model's space, its node's world transform baked in.
    Baked,
    /// In its node's own space, placed by the node's palette slot.
    Rigid(u16),
    /// In its skin's bind space, placed by the joints' slots from this one on.
    Skinned { base: u16, joints: usize },
}

/// Reads every buffer once: two entries that name the same file share its bytes, and all
/// of them together may hold no more than a model may (ADR-0107), so a short list can't
/// read one big file over and over.
fn load_buffers(
    doc: &Document,
    bin: Option<&[u8]>,
    resources: &mut dyn Resources,
) -> Result<Vec<Rc<[u8]>>> {
    let mut out: Vec<Rc<[u8]>> = Vec::with_capacity(doc.buffers.len());
    // Keyed by the decoded name, so `a.bin` and `%61.bin` are one read.
    let mut read: BTreeMap<String, Rc<[u8]>> = BTreeMap::new();
    let mut total: u64 = 0;
    let mut count = |bytes: &[u8]| {
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_MODEL_BYTES {
            return Err(LoadError::new(format!(
                "the model's buffers hold more than {} MiB",
                MAX_MODEL_BYTES / (1024 * 1024)
            )));
        }
        Ok(())
    };
    for (index, buffer) in doc.buffers.iter().enumerate() {
        let bytes: Rc<[u8]> = match &buffer.uri {
            Some(uri) => {
                let key = if uri.starts_with("data:") {
                    uri.clone()
                } else {
                    percent_decode(uri)
                };
                match read.get(&key) {
                    Some(bytes) => Rc::clone(bytes),
                    None => {
                        let bytes: Rc<[u8]> = read_uri(uri, resources)?.into();
                        count(&bytes)?;
                        read.insert(key, Rc::clone(&bytes));
                        bytes
                    }
                }
            }
            None if index == 0 => {
                let bytes: Rc<[u8]> = bin
                    .ok_or_else(|| {
                        LoadError::new("buffer 0 has no URI and the file no binary chunk")
                    })?
                    .into();
                // The `.glb`'s own binary chunk counts toward the total too.
                count(&bytes)?;
                bytes
            }
            None => return Err(LoadError::new(format!("buffer {index} has no URI"))),
        };
        if bytes.len() < buffer.byte_length {
            return Err(LoadError::new(format!(
                "buffer {index} has {} bytes, fewer than its byteLength of {}",
                bytes.len(),
                buffer.byte_length
            )));
        }
        out.push(bytes);
    }
    Ok(out)
}

fn read_uri(uri: &str, resources: &mut dyn Resources) -> Result<Vec<u8>> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (header, payload) = data
            .split_once(',')
            .ok_or_else(|| LoadError::new("a data URI has no comma"))?;
        if !header.ends_with(";base64") {
            return Err(LoadError::new("a data URI is not base64"));
        }
        return base64(payload).ok_or_else(|| LoadError::new("a data URI's base64 is not valid"));
    }
    if uri.contains("://") {
        return Err(LoadError::new(format!(
            "`{uri}` is not a file beside the model"
        )));
    }
    let path = percent_decode(uri);
    // A file the model names is relative to it and stays below its folder: no `..`, no
    // absolute or drive path, no `:` and no device name, whatever root the model is in.
    check_asset_path(AssetRoot::Collection, &path).map_err(|reason| {
        LoadError::new(format!("`{uri}` is not a file beside the model: {reason}"))
    })?;
    resources.read(&path)
}

fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = uri.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64, padding optional, whitespace skipped.
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A column-major 4x4 matrix, as glTF stores them.
type Mat4 = [f64; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn multiply(a: &Mat4, b: &Mat4) -> Mat4 {
    std::array::from_fn(|i| {
        let (c, r) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum()
    })
}

const UNIT: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

fn local(node: &Node) -> Mat4 {
    if let Some(m) = node.matrix {
        return m;
    }
    let [tx, ty, tz] = node.translation.unwrap_or([0.0; 3]);
    let [x, y, z, w] = node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let [sx, sy, sz] = node.scale.unwrap_or([1.0; 3]);
    let r = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
    ];
    [
        r[0] * sx,
        r[1] * sx,
        r[2] * sx,
        0.0,
        r[3] * sy,
        r[4] * sy,
        r[5] * sy,
        0.0,
        r[6] * sz,
        r[7] * sz,
        r[8] * sz,
        0.0,
        tx,
        ty,
        tz,
        1.0,
    ]
}

fn point(m: &Mat4, p: [f32; 3]) -> [f64; 3] {
    let p = p.map(f64::from);
    std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

fn direction(m: &[f64; 9], v: [f32; 3]) -> [f64; 3] {
    let v = v.map(f64::from);
    std::array::from_fn(|r| m[r] * v[0] + m[3 + r] * v[1] + m[6 + r] * v[2])
}

fn linear(m: &Mat4) -> [f64; 9] {
    [m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]]
}

fn determinant(m: &[f64; 9]) -> f64 {
    m[0] * (m[4] * m[8] - m[7] * m[5]) - m[3] * (m[1] * m[8] - m[7] * m[2])
        + m[6] * (m[1] * m[5] - m[4] * m[2])
}

/// The inverse transpose of a 3x3 (column-major), for normals; the identity when it is
/// singular.
fn normal_matrix(m: &[f64; 9]) -> [f64; 9] {
    let det = determinant(m);
    if det.abs() < 1e-300 || !det.is_finite() {
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    }
    let a = |r: usize, c: usize| m[c * 3 + r];
    // cofactor(r, c) / det, stored column-major: element (r, c) of the inverse transpose.
    let cof = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)
    };
    std::array::from_fn(|i| {
        let (c, r) = (i / 3, i % 3);
        cof(r, c) / det
    })
}

fn normalize(v: [f64; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < 1e-30 || !length.is_finite() {
        return [0.0, 0.0, 1.0];
    }
    v.map(|c| (c / length) as f32)
}

/// glTF's y up to the scene's y down.
fn flip(v: [f32; 3]) -> [f32; 3] {
    [v[0], -v[1], v[2]]
}

struct Builder<'a> {
    doc: &'a Document,
    buffers: &'a [Rc<[u8]>],
    resources: &'a mut dyn Resources,
    textures: Vec<ModelTexture>,
    /// Each image already decoded, by its index and how it is read.
    texture_of: BTreeMap<(usize, bool), usize>,
    /// Each glTF material already built (`None` for the default material).
    materials: BTreeMap<Option<usize>, usize>,
    material_list: Vec<Material>,
    primitives: Vec<Primitive>,
    ignored: &'a mut Ignored,
    /// What the model holds so far, checked against ADR-0107's caps as it grows, so a
    /// file that would pass them only after building something huge is stopped early.
    triangles: usize,
    bytes: u64,
    /// Node instances visited: a hierarchy that names the same children many times fans
    /// out, and every instance of every mesh costs time even when it adds nothing.
    visits: usize,
    /// The rig, for a file with skins or animations.
    rig: Option<Tree>,
}

/// The most node and primitive instances a scene may expand to.
const MAX_NODE_VISITS: usize = 100_000;

/// The most elements an accessor may hold: a list of the most triangles a model may have,
/// with room for a strip's two leading indices.
const MAX_ACCESSOR_COUNT: usize = 3 * MAX_TRIANGLES + 2;

/// How deep a node hierarchy may go: a cycle in a malformed file stops here.
const MAX_DEPTH: usize = 64;

impl Builder<'_> {
    fn node(&mut self, index: usize, parent: Mat4, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(LoadError::new(
                "the node hierarchy is deeper than 64, or has a cycle",
            ));
        }
        self.visit()?;
        let node = self
            .doc
            .nodes
            .get(index)
            .ok_or_else(|| LoadError::new(format!("node {index} doesn't exist")))?;
        if node.weights.is_some() {
            self.ignored.0.insert("morph targets");
        }
        let world = multiply(&parent, &local(node));
        if let Some(mesh) = node.mesh {
            let placed = self.placed(index, node.skin)?;
            self.mesh(mesh, &world, placed)?;
        }
        for &child in &node.children {
            self.node(child, world, depth + 1)?;
        }
        Ok(())
    }

    /// Counts one more node or primitive instance, and stops a scene that expands past
    /// [`MAX_NODE_VISITS`] of them, even when each adds nothing.
    fn visit(&mut self) -> Result<()> {
        self.visits += 1;
        if self.visits > MAX_NODE_VISITS {
            return Err(LoadError::new(format!(
                "the scene expands to more than {MAX_NODE_VISITS} node and primitive instances"
            )));
        }
        Ok(())
    }

    /// Where the mesh of node `index`, maybe skinned by `skin`, keeps its vertices,
    /// handing out its palette slots the first time.
    fn placed(&mut self, index: usize, skin: Option<usize>) -> Result<Placed> {
        let Some(tree) = self.rig.as_mut() else {
            if skin.is_some() {
                self.ignored.0.insert("skins");
            }
            return Ok(Placed::Baked);
        };
        let too_many = || {
            LoadError::new(format!(
                "the model's rig needs more than {} palette slots",
                rig::MAX_SLOTS
            ))
        };
        if let Some(skin) = skin {
            let def = self
                .doc
                .skins
                .get(skin)
                .ok_or_else(|| LoadError::new(format!("skin {skin} doesn't exist")))?;
            if def.joints.len() > rig::MAX_JOINTS {
                return Err(LoadError::new(format!(
                    "skin {skin} has {} joints, more than the {} a skin may have",
                    def.joints.len(),
                    rig::MAX_JOINTS
                )));
            }
            if let Some(&base) = tree.skin_base.get(&skin) {
                return Ok(Placed::Skinned {
                    base,
                    joints: def.joints.len(),
                });
            }
            let inverse = match def.inverse_bind_matrices {
                Some(accessor) => {
                    let values = self.read(accessor, 16, false)?;
                    if values.len() < def.joints.len() * 16 || values.iter().any(|v| !v.is_finite()) {
                        return Err(LoadError::new(format!(
                            "skin {skin}'s inverse bind matrices don't match its joints"
                        )));
                    }
                    values
                }
                None => IDENTITY.repeat(def.joints.len()),
            };
            let tree = self.rig.as_mut().expect("a rig");
            let base = u16::try_from(tree.slots.len()).map_err(|_| too_many())?;
            for (j, &joint) in def.joints.iter().enumerate() {
                let node = tree
                    .rig_of
                    .get(joint)
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        LoadError::new(format!("skin {skin} names node {joint}, which isn't in the scene"))
                    })?;
                tree.slots.push(Slot {
                    node,
                    inverse_bind: std::array::from_fn(|i| inverse[j * 16 + i]),
                });
            }
            if tree.slots.len() > rig::MAX_SLOTS {
                return Err(too_many());
            }
            tree.skin_base.insert(skin, base);
            return Ok(Placed::Skinned {
                base,
                joints: def.joints.len(),
            });
        }
        if let Some(&slot) = tree.node_slot.get(&index) {
            return Ok(Placed::Rigid(slot));
        }
        let node = tree.rig_of[index].expect("a node in the scene's tree");
        let slot = u16::try_from(tree.slots.len()).map_err(|_| too_many())?;
        if usize::from(slot) >= rig::MAX_SLOTS {
            return Err(too_many());
        }
        tree.slots.push(Slot {
            node,
            inverse_bind: IDENTITY,
        });
        tree.node_slot.insert(index, slot);
        Ok(Placed::Rigid(slot))
    }

    /// The rig, with the file's animations as its clips.
    fn finish_rig(&mut self, tree: Tree) -> Result<Rig> {
        let mut clips = Vec::new();
        for (a, animation) in self.doc.animations.iter().enumerate() {
            let mut channels = Vec::new();
            for c in &animation.channels {
                let path = match c.target.path.as_str() {
                    "translation" => Path::Translation,
                    "rotation" => Path::Rotation,
                    "scale" => Path::Scale,
                    _ => {
                        self.ignored.0.insert("morph targets");
                        continue;
                    }
                };
                // A channel on a node outside the scene moves nothing drawn.
                let Some(node) = c
                    .target
                    .node
                    .and_then(|n| tree.rig_of.get(n).copied().flatten())
                else {
                    continue;
                };
                let sampler = animation.samplers.get(c.sampler).ok_or_else(|| {
                    LoadError::new(format!("animation {a} names sampler {} it doesn't have", c.sampler))
                })?;
                let interpolation = match sampler.interpolation.as_deref() {
                    None | Some("LINEAR") => Interpolation::Linear,
                    Some("STEP") => Interpolation::Step,
                    Some("CUBICSPLINE") => Interpolation::CubicSpline,
                    Some(other) => {
                        return Err(LoadError::new(format!(
                            "animation {a} has the interpolation {other}"
                        )));
                    }
                };
                let width = if path == Path::Rotation { 4 } else { 3 };
                channels.push(Channel {
                    node,
                    path,
                    interpolation,
                    times: self.floats(sampler.input, 1)?,
                    values: self.floats(sampler.output, width)?,
                });
            }
            clips.push(Clip {
                name: animation
                    .name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| format!("animation {a}")),
                channels,
                duration: 0.0,
            });
        }
        Rig::new(tree.nodes, tree.slots, clips).map_err(|err| LoadError::new(err.to_string()))
    }

    fn mesh(&mut self, index: usize, world: &Mat4, placed: Placed) -> Result<()> {
        let mesh = self
            .doc
            .meshes
            .get(index)
            .ok_or_else(|| LoadError::new(format!("mesh {index} doesn't exist")))?;
        let linear = linear(world);
        let normals = normal_matrix(&linear);
        // A mirroring transform turns triangles inside out; swap them back. A skin's
        // joints place its mesh, not its node.
        let mirrored = !matches!(placed, Placed::Skinned { .. }) && determinant(&linear) < 0.0;
        for p in &mesh.primitives {
            self.visit()?;
            if p.targets.as_ref().is_some_and(|t| !t.is_empty()) {
                self.ignored.0.insert("morph targets");
            }
            let order: Vec<u32> = match p.mode {
                4..=6 => {
                    let count = self.accessor_count(p.attributes.get("POSITION"))?;
                    // Refuse a vertex count past the caps before anything is built from it.
                    let bytes = (count as u64).saturating_mul(VERTEX_BYTES);
                    if count > MAX_ACCESSOR_COUNT
                        || self.bytes.saturating_add(bytes) > MAX_MODEL_BYTES
                    {
                        return Err(LoadError::new(format!(
                            "a primitive of mesh {index} has {count} vertices, more than a model may hold"
                        )));
                    }
                    let raw = match p.indices {
                        Some(i) => self.indices(i)?,
                        None => (0..count as u32).collect(),
                    };
                    triangulate(&raw, p.mode)
                }
                _ => {
                    self.ignored.0.insert("points and lines");
                    continue;
                }
            };
            if order.is_empty() {
                // Nothing to draw.
                continue;
            }
            let positions = self.floats(
                *p.attributes.get("POSITION").ok_or_else(|| {
                    LoadError::new(format!("a primitive of mesh {index} has no POSITION"))
                })?,
                3,
            )?;
            let count = positions.len() / 3;
            let normal_data = match p.attributes.get("NORMAL") {
                Some(&a) => Some(self.floats(a, 3)?),
                None => None,
            };
            let tangent_data = match p.attributes.get("TANGENT") {
                Some(&a) => Some(self.floats(a, 4)?),
                None => None,
            };
            let uv_data = match p.attributes.get("TEXCOORD_0") {
                Some(&a) => Some(self.floats(a, 2)?),
                None => None,
            };
            let color_data = match p.attributes.get("COLOR_0") {
                Some(&a) => Some(self.colors(a)?),
                None => None,
            };
            let skin = match placed {
                Placed::Skinned { base, joints } => {
                    let (Some(&j), Some(&w)) =
                        (p.attributes.get("JOINTS_0"), p.attributes.get("WEIGHTS_0"))
                    else {
                        return Err(LoadError::new(format!(
                            "a skinned primitive of mesh {index} has no JOINTS_0 or WEIGHTS_0"
                        )));
                    };
                    let names = self.read(j, 4, false)?;
                    let weights = self.floats(w, 4)?;
                    if names.len() != count * 4 || weights.len() != count * 4 {
                        return Err(LoadError::new(format!(
                            "a primitive of mesh {index} has a JOINTS_0 or WEIGHTS_0 of another length than its POSITION"
                        )));
                    }
                    if let Some(bad) = names.iter().find(|&&n| n < 0.0 || n as usize >= joints) {
                        return Err(LoadError::new(format!(
                            "a primitive of mesh {index} names joint {bad} of its skin's {joints}"
                        )));
                    }
                    Some((base, names, weights))
                }
                _ => None,
            };
            // Every attribute has one element per vertex.
            for (name, data, n) in [
                ("NORMAL", &normal_data, 3),
                ("TANGENT", &tangent_data, 4),
                ("TEXCOORD_0", &uv_data, 2),
                ("COLOR_0", &color_data, 4),
            ] {
                if data.as_ref().is_some_and(|d| d.len() != count * n) {
                    return Err(LoadError::new(format!(
                        "a primitive of mesh {index} has a {name} of another length than its POSITION"
                    )));
                }
            }
            let triangles = order.len() / 3;
            // Flat normals give each corner its own vertex, so count the larger.
            let vertices = count.max(order.len());
            let bytes = vertices as u64 * VERTEX_BYTES + order.len() as u64 * 4;
            self.triangles = self.triangles.saturating_add(triangles);
            self.bytes = self.bytes.saturating_add(bytes);
            if self.triangles > MAX_TRIANGLES {
                return Err(LoadError::new(format!(
                    "the model has more than {MAX_TRIANGLES} triangles"
                )));
            }
            if self.bytes > MAX_MODEL_BYTES {
                return Err(LoadError::new(format!(
                    "the model takes more than {} MiB",
                    MAX_MODEL_BYTES / (1024 * 1024)
                )));
            }
            if order.iter().any(|&i| i as usize >= count) {
                return Err(LoadError::new(format!(
                    "a primitive of mesh {index} names a vertex it doesn't have"
                )));
            }
            let mut vertices: Vec<ModelVertex> = (0..count)
                .map(|i| {
                    fn at(data: &[f32], i: usize, n: usize) -> &[f32] {
                        &data[i * n..i * n + n]
                    }
                    let raw = [positions[i * 3], positions[i * 3 + 1], positions[i * 3 + 2]];
                    // A rigged mesh keeps its own space: its palette slots place it.
                    let (pos, normals, linear, mirrored) = match placed {
                        Placed::Baked => (point(world, raw), normals, linear, mirrored),
                        _ => (raw.map(f64::from), UNIT, UNIT, false),
                    };
                    let normal = normal_data
                        .as_ref()
                        .map(|d| {
                            normalize(direction(
                                &normals,
                                [at(d, i, 3)[0], at(d, i, 3)[1], at(d, i, 3)[2]],
                            ))
                        })
                        .unwrap_or([0.0, 0.0, 1.0]);
                    let tangent = tangent_data.as_ref().map_or([1.0, 0.0, 0.0, 1.0], |d| {
                        let t = at(d, i, 4);
                        let dir = normalize(direction(&linear, [t[0], t[1], t[2]]));
                        let flipped = flip(dir);
                        // A mirroring transform flips the bitangent's handedness.
                        let w = if mirrored { -t[3] } else { t[3] };
                        [flipped[0], flipped[1], flipped[2], w]
                    });
                    let (joints, weights) = match (&placed, &skin) {
                        (Placed::Rigid(slot), _) => ([*slot, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
                        (_, Some((base, names, weights))) => {
                            let w = at(weights, i, 4);
                            let sum: f32 = w.iter().sum();
                            let joints = std::array::from_fn(|k| base + names[i * 4 + k] as u16);
                            if sum > 0.0 {
                                (joints, std::array::from_fn(|k| w[k] / sum))
                            } else {
                                // No weight at all: the first joint holds it.
                                (joints, [1.0, 0.0, 0.0, 0.0])
                            }
                        }
                        _ => ([0; 4], [0.0; 4]),
                    };
                    let uv = uv_data
                        .as_ref()
                        .map_or([0.0, 0.0], |d| [at(d, i, 2)[0], at(d, i, 2)[1]]);
                    let color = color_data.as_ref().map_or([1.0; 4], |d| {
                        let c = at(d, i, 4);
                        [c[0], c[1], c[2], c[3]]
                    });
                    ModelVertex {
                        position: flip(pos.map(|c| c as f32)),
                        normal: flip(normal),
                        tangent,
                        uv,
                        color,
                        joints,
                        weights,
                    }
                })
                .collect();
            let mut indices = Vec::with_capacity(order.len());
            for t in order.chunks_exact(3) {
                // Flipping y turns each triangle over, and a mirror turns it over again.
                if mirrored {
                    indices.extend([t[0], t[1], t[2]]);
                } else {
                    indices.extend([t[0], t[2], t[1]]);
                }
            }
            if normal_data.is_none() {
                (vertices, indices) = flat(vertices, &indices);
            }
            let material = self.material(p.material)?;
            let mut primitive = Primitive {
                vertices,
                indices,
                material,
            };
            if tangent_data.is_none() {
                compute_tangents(&mut primitive);
            }
            self.primitives.push(primitive);
        }
        Ok(())
    }

    fn accessor_count(&self, accessor: Option<&usize>) -> Result<usize> {
        let index = accessor.ok_or_else(|| LoadError::new("a primitive has no POSITION"))?;
        self.doc
            .accessors
            .get(*index)
            .map(|a| a.count)
            .ok_or_else(|| LoadError::new(format!("accessor {index} doesn't exist")))
    }

    /// The raw components of an accessor, as `f64`, `components` per element: what every
    /// other read is built on.
    /// A VEC3 is read where `components` is 4 only when `widen` (`COLOR_0`'s RGB).
    fn read(&self, index: usize, components: usize, widen: bool) -> Result<Vec<f64>> {
        let accessor = self
            .doc
            .accessors
            .get(index)
            .ok_or_else(|| LoadError::new(format!("accessor {index} doesn't exist")))?;
        if accessor.sparse.is_some() {
            return Err(LoadError::new(format!(
                "accessor {index} is sparse, which JStream can't read"
            )));
        }
        let have = match accessor.kind.as_str() {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            "MAT4" => 16,
            other => return Err(LoadError::new(format!("accessor {index} is a {other}"))),
        };
        if have != components && !(widen && components == 4 && have == 3) {
            return Err(LoadError::new(format!(
                "accessor {index} has {have} components, not {components}"
            )));
        }
        let size = match accessor.component_type {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            other => {
                return Err(LoadError::new(format!(
                    "accessor {index} has component type {other}"
                )));
            }
        };
        if accessor.count > MAX_ACCESSOR_COUNT {
            return Err(LoadError::new(format!(
                "accessor {index} has {} elements, more than a model may hold",
                accessor.count
            )));
        }
        let Some(view_index) = accessor.buffer_view else {
            // No view: all zeros, as the spec says.
            return Ok(vec![0.0; accessor.count * have]);
        };
        let view = self
            .doc
            .buffer_views
            .get(view_index)
            .ok_or_else(|| LoadError::new(format!("buffer view {view_index} doesn't exist")))?;
        let buffer = self
            .buffers
            .get(view.buffer)
            .ok_or_else(|| LoadError::new(format!("buffer {} doesn't exist", view.buffer)))?;
        let element = size * have;
        let stride = view.byte_stride.unwrap_or(element).max(element);
        let start = view.byte_offset.saturating_add(accessor.byte_offset);
        let view_end = view
            .byte_offset
            .saturating_add(view.byte_length)
            .min(buffer.len());
        let last = match accessor.count {
            0 => start,
            n => start
                .checked_add((n - 1).saturating_mul(stride))
                .and_then(|v| v.checked_add(element))
                .unwrap_or(usize::MAX),
        };
        if last > view_end {
            return Err(LoadError::new(format!(
                "accessor {index} runs past its buffer view"
            )));
        }
        let mut out = Vec::with_capacity(accessor.count * have);
        for i in 0..accessor.count {
            let base = start + i * stride;
            for c in 0..have {
                let at = base + c * size;
                let b = &buffer[at..at + size];
                let raw = match accessor.component_type {
                    5120 => f64::from(b[0] as i8),
                    5121 => f64::from(b[0]),
                    5122 => f64::from(i16::from_le_bytes([b[0], b[1]])),
                    5123 => f64::from(u16::from_le_bytes([b[0], b[1]])),
                    5125 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    _ => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                };
                let value = if accessor.normalized {
                    match accessor.component_type {
                        5120 => (raw / 127.0).max(-1.0),
                        5121 => raw / 255.0,
                        5122 => (raw / 32767.0).max(-1.0),
                        5123 => raw / 65535.0,
                        _ => raw,
                    }
                } else {
                    raw
                };
                out.push(value);
            }
        }
        Ok(out)
    }

    fn floats(&self, index: usize, components: usize) -> Result<Vec<f32>> {
        self.finite(index, self.read(index, components, false)?)
    }

    fn finite(&self, index: usize, values: Vec<f64>) -> Result<Vec<f32>> {
        if values.iter().any(|v| !v.is_finite()) {
            return Err(LoadError::new(format!(
                "accessor {index} holds a value that isn't finite"
            )));
        }
        Ok(values.into_iter().map(|v| v as f32).collect())
    }

    /// `COLOR_0`, as RGBA: a VEC3 gains alpha 1.
    fn colors(&self, index: usize) -> Result<Vec<f32>> {
        let have = self.doc.accessors.get(index).map(|a| a.kind.as_str());
        let values = self.finite(index, self.read(index, 4, true)?)?;
        if have == Some("VEC3") {
            return Ok(values
                .chunks_exact(3)
                .flat_map(|c| [c[0], c[1], c[2], 1.0])
                .collect());
        }
        Ok(values)
    }

    fn indices(&self, index: usize) -> Result<Vec<u32>> {
        Ok(self
            .read(index, 1, false)?
            .into_iter()
            .map(|v| v as u32)
            .collect())
    }

    fn material(&mut self, index: Option<usize>) -> Result<usize> {
        if let Some(&built) = self.materials.get(&index) {
            return Ok(built);
        }
        let material = match index {
            None => Material {
                name: "default".into(),
                // glTF's default material: white, with the schema's metallic and
                // roughness factors of 1.
                metallic: 1.0,
                roughness: 1.0,
                ..Material::default()
            },
            Some(i) => {
                let def = self
                    .doc
                    .materials
                    .get(i)
                    .ok_or_else(|| LoadError::new(format!("material {i} doesn't exist")))?;
                let pbr = &def.pbr_metallic_roughness;
                let strength = def
                    .extensions
                    .get("KHR_materials_emissive_strength")
                    .and_then(|e| e.get("emissiveStrength"))
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(1.0) as f32;
                let alpha_mode = match def.alpha_mode.as_deref() {
                    Some("MASK") => AlphaMode::Mask {
                        cutoff: def.alpha_cutoff,
                    },
                    Some("BLEND") => AlphaMode::Blend,
                    _ => AlphaMode::Opaque,
                };
                Material {
                    name: def.name.clone().unwrap_or_else(|| format!("material {i}")),
                    base_color: pbr.base_color_factor,
                    base_color_texture: self.texture(pbr.base_color_texture.as_ref(), true)?,
                    metallic: pbr.metallic_factor,
                    roughness: pbr.roughness_factor,
                    metallic_roughness_texture: self
                        .texture(pbr.metallic_roughness_texture.as_ref(), false)?,
                    normal_texture: self.texture(def.normal_texture.as_ref(), false)?,
                    normal_scale: def
                        .normal_texture
                        .as_ref()
                        .and_then(|t| t.scale)
                        .unwrap_or(1.0),
                    occlusion_texture: self.texture(def.occlusion_texture.as_ref(), false)?,
                    occlusion_strength: def
                        .occlusion_texture
                        .as_ref()
                        .and_then(|t| t.strength)
                        .unwrap_or(1.0),
                    emissive: def.emissive_factor.map(|c| c * strength),
                    emissive_texture: self.texture(def.emissive_texture.as_ref(), true)?,
                    alpha_mode,
                    double_sided: def.double_sided,
                    unlit: def.extensions.contains_key("KHR_materials_unlit"),
                }
            }
        };
        let built = self.material_list.len();
        self.material_list.push(material);
        self.materials.insert(index, built);
        Ok(built)
    }

    fn texture(&mut self, info: Option<&TextureInfo>, srgb: bool) -> Result<Option<TextureRef>> {
        let Some(info) = info else {
            return Ok(None);
        };
        if info.tex_coord != 0 {
            self.ignored.0.insert("second texture coordinates");
        }
        let Some(image) = self.doc.textures.get(info.index).and_then(|t| t.source) else {
            // A texture with no image (or only an extension's): drawn without it.
            self.ignored
                .0
                .insert("textures without a PNG or JPEG image");
            return Ok(None);
        };
        if let Some(&built) = self.texture_of.get(&(image, srgb)) {
            return Ok(Some(TextureRef { texture: built }));
        }
        if self.textures.len() >= MAX_TEXTURES {
            return Err(LoadError::new(format!(
                "the model has more than {MAX_TEXTURES} textures"
            )));
        }
        let def = self
            .doc
            .images
            .get(image)
            .ok_or_else(|| LoadError::new(format!("image {image} doesn't exist")))?;
        let bytes = match (&def.uri, def.buffer_view) {
            (Some(uri), _) => read_uri(uri, self.resources)?,
            (None, Some(view)) => {
                let v =
                    self.doc.buffer_views.get(view).ok_or_else(|| {
                        LoadError::new(format!("buffer view {view} doesn't exist"))
                    })?;
                let buffer = self
                    .buffers
                    .get(v.buffer)
                    .ok_or_else(|| LoadError::new(format!("buffer {} doesn't exist", v.buffer)))?;
                buffer
                    .get(v.byte_offset..v.byte_offset.saturating_add(v.byte_length))
                    .ok_or_else(|| LoadError::new(format!("image {image} runs past its buffer")))?
                    .to_vec()
            }
            (None, None) => return Err(LoadError::new(format!("image {image} has no data"))),
        };
        let (width, height, rgba) = self.resources.decode(&bytes, &format!("image {image}"))?;
        let (width, height, rgba) = shrink(width, height, rgba);
        let color = if srgb {
            TextureColor::Srgb
        } else {
            TextureColor::Linear
        };
        let texture = ModelTexture::new(width, height, rgba, color)
            .map_err(|err| LoadError::new(format!("image {image}: {err}")))?;
        let texture_bytes: u64 = texture.levels().iter().map(|l| l.len() as u64).sum();
        self.bytes = self.bytes.saturating_add(texture_bytes);
        if self.bytes > MAX_MODEL_BYTES {
            return Err(LoadError::new(format!(
                "the model takes more than {} MiB",
                MAX_MODEL_BYTES / (1024 * 1024)
            )));
        }
        let built = self.textures.len();
        self.textures.push(texture);
        self.texture_of.insert((image, srgb), built);
        Ok(Some(TextureRef { texture: built }))
    }
}

/// Triangles from a list (mode 4), strip (5) or fan (6) of indices.
fn triangulate(raw: &[u32], mode: u32) -> Vec<u32> {
    match mode {
        5 => (2..raw.len())
            .flat_map(|i| {
                if i % 2 == 0 {
                    [raw[i - 2], raw[i - 1], raw[i]]
                } else {
                    [raw[i - 1], raw[i - 2], raw[i]]
                }
            })
            .collect(),
        6 => (2..raw.len())
            .flat_map(|i| [raw[0], raw[i - 1], raw[i]])
            .collect(),
        _ => raw[..raw.len() - raw.len() % 3].to_vec(),
    }
}

/// Every triangle with its own three vertices and its face normal: what glTF says to do
/// for a mesh without normals.
fn flat(vertices: Vec<ModelVertex>, indices: &[u32]) -> (Vec<ModelVertex>, Vec<u32>) {
    let mut out = Vec::with_capacity(indices.len());
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| vertices[i as usize]);
        let e1 = [0, 1, 2].map(|i| f64::from(b.position[i] - a.position[i]));
        let e2 = [0, 1, 2].map(|i| f64::from(c.position[i] - a.position[i]));
        let n = normalize([
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ]);
        for mut v in [a, b, c] {
            v.normal = n;
            out.push(v);
        }
    }
    let count = out.len() as u32;
    (out, (0..count).collect())
}

/// Halves a texture until neither side is past [`MAX_TEXTURE_SIDE`].
fn shrink(mut width: u32, mut height: u32, mut rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    while width > MAX_TEXTURE_SIDE || height > MAX_TEXTURE_SIDE {
        let (nw, nh) = ((width / 2).max(1), (height / 2).max(1));
        let mut out = Vec::with_capacity((nw * nh * 4) as usize);
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(width - 1);
                        let sy = (y * 2 + dy).min(height - 1);
                        sum += u32::from(rgba[((sy * width + sx) * 4 + c) as usize]);
                    }
                    out.push((sum / 4) as u8);
                }
            }
        }
        (width, height, rgba) = (nw, nh, out);
    }
    (width, height, rgba)
}

#[cfg(test)]
mod tests;
