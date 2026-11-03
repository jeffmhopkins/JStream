//! The loader against small files built here: a `.glb` and `.gltf` with data URIs, so
//! every case is in the test and none needs a fixture on disk.

use super::{Ignored, LoadError, Resources, load};
use jstream_media::model::{AlphaMode, Model};
use serde_json::{Value, json};

/// Files a test's `.gltf` names, and a decoder that turns any bytes into a 2x2 image of
/// their first byte.
#[derive(Default)]
struct Files(Vec<(String, Vec<u8>)>);

impl Resources for Files {
    fn read(&mut self, uri: &str) -> Result<Vec<u8>, LoadError> {
        self.0
            .iter()
            .find(|(name, _)| name == uri)
            .map(|(_, bytes)| bytes.clone())
            .ok_or_else(|| LoadError(format!("no file {uri}")))
    }

    fn decode(&mut self, bytes: &[u8], _: &str) -> Result<(u32, u32, Vec<u8>), LoadError> {
        let v = bytes.first().copied().unwrap_or(0);
        Ok((2, 2, vec![v; 16]))
    }
}

fn f32s(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn u16s(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// One triangle facing +z (toward the viewer) in glTF's y-up axes, counter-clockwise
/// seen from the front: its JSON, with `extra` merged in at the top, and its buffer.
fn triangle(extra: Value, with_normals: bool) -> (Value, Vec<u8>) {
    let mut bin = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    let normals_at = bin.len();
    if with_normals {
        bin.extend(f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
    }
    let indices_at = bin.len();
    bin.extend(u16s(&[0, 1, 2]));
    bin.extend([0, 0]);
    let mut attributes = json!({ "POSITION": 0 });
    let mut accessors = vec![json!({
        "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
        "min": [0, 0, 0], "max": [1, 1, 0]
    })];
    let mut views = vec![json!({ "buffer": 0, "byteOffset": 0, "byteLength": 36 })];
    if with_normals {
        attributes["NORMAL"] = json!(accessors.len());
        accessors.push(json!({
            "bufferView": views.len(), "componentType": 5126, "count": 3, "type": "VEC3"
        }));
        views.push(json!({ "buffer": 0, "byteOffset": normals_at, "byteLength": 36 }));
    }
    let index_accessor = accessors.len();
    accessors.push(json!({
        "bufferView": views.len(), "componentType": 5123, "count": 3, "type": "SCALAR"
    }));
    views.push(json!({ "buffer": 0, "byteOffset": indices_at, "byteLength": 6 }));
    let mut doc = json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": attributes, "indices": index_accessor }] }],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "byteLength": bin.len() }],
    });
    if let (Some(doc), Value::Object(extra)) = (doc.as_object_mut(), extra) {
        for (key, value) in extra {
            doc.insert(key, value);
        }
    }
    (doc, bin)
}

fn glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).expect("serialises");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend(0x4654_6C67u32.to_le_bytes());
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((json.len() as u32).to_le_bytes());
    out.extend(0x4E4F_534Au32.to_le_bytes());
    out.extend(json);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(0x004E_4942u32.to_le_bytes());
    out.extend(bin);
    out
}

fn load_glb(doc: &Value, bin: &[u8]) -> Result<(Model, Ignored), LoadError> {
    load(&glb(doc, bin), &mut Files::default())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// For each triangle, `(b − a) × (c − a)` against its first vertex's normal: positive
/// when the winding faces along the normal, the model's convention.
fn facing(model: &Model) -> Vec<f32> {
    let p = &model.primitives()[0];
    p.indices
        .chunks(3)
        .map(|t| {
            let at = |i: u32| p.vertices[i as usize];
            let (a, b, c) = (at(t[0]), at(t[1]), at(t[2]));
            let e1 = [0, 1, 2].map(|i| b.position[i] - a.position[i]);
            let e2 = [0, 1, 2].map(|i| c.position[i] - a.position[i]);
            let cross = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            (0..3).map(|i| cross[i] * a.normal[i]).sum()
        })
        .collect()
}

#[test]
fn a_glb_triangle_lands_in_the_scenes_axes_facing_the_viewer() {
    let (doc, bin) = triangle(json!({}), true);
    let (model, ignored) = load_glb(&doc, &bin).expect("loads");
    assert_eq!(model.triangles(), 1);
    let p = &model.primitives()[0];
    // glTF's (0, 1, 0), up, is the scene's (0, -1, 0).
    assert!(p.vertices.iter().any(|v| v.position == [0.0, -1.0, 0.0]));
    assert!(p.vertices.iter().all(|v| v.normal == [0.0, 0.0, 1.0]));
    // Its winding was swapped with the flip, so it still faces along its normal.
    assert!(facing(&model)[0] > 0.0);
    assert_eq!(ignored, Ignored::default());
    // The default material: white, metallic and roughness 1.
    assert_eq!(model.materials()[0].metallic, 1.0);
}

#[test]
fn a_gltf_reads_a_data_uri_and_bakes_its_node_transforms() {
    let (mut doc, bin) = triangle(json!({}), true);
    doc["buffers"][0]["uri"] = json!(format!(
        "data:application/octet-stream;base64,{}",
        base64(&bin)
    ));
    doc["nodes"] = json!([
        { "children": [1], "translation": [10.0, 0.0, 0.0] },
        { "mesh": 0, "scale": [2.0, 2.0, 2.0] }
    ]);
    let json = serde_json::to_vec(&doc).expect("serialises");
    let (model, _) = load(&json, &mut Files::default()).expect("loads");
    let bounds = model.bounds();
    assert_eq!(bounds.min, [10.0, -2.0, 0.0]);
    assert_eq!(bounds.max, [12.0, 0.0, 0.0]);
}

#[test]
fn a_gltf_reads_a_buffer_file_beside_it_and_never_a_url() {
    let (mut doc, bin) = triangle(json!({}), true);
    doc["buffers"][0]["uri"] = json!("mesh%20data.bin");
    let json = serde_json::to_vec(&doc).expect("serialises");
    let mut files = Files(vec![("mesh data.bin".into(), bin)]);
    assert!(load(&json, &mut files).is_ok());
    doc["buffers"][0]["uri"] = json!("https://example.com/mesh.bin");
    let json = serde_json::to_vec(&doc).expect("serialises");
    let err = load(&json, &mut files).expect_err("a URL is not a file");
    assert!(err.0.contains("not a file beside the model"), "{err:?}");
}

#[test]
fn a_mirroring_node_keeps_its_faces_out() {
    let (mut doc, bin) = triangle(json!({}), true);
    doc["nodes"][0]["scale"] = json!([-1.0, 1.0, 1.0]);
    let (model, _) = load_glb(&doc, &bin).expect("loads");
    assert!(facing(&model)[0] > 0.0);
    assert!(
        model.primitives()[0]
            .vertices
            .iter()
            .all(|v| v.normal == [0.0, 0.0, 1.0])
    );
}

#[test]
fn a_mesh_without_normals_gets_flat_ones() {
    let (doc, bin) = triangle(json!({}), false);
    let (model, _) = load_glb(&doc, &bin).expect("loads");
    for v in &model.primitives()[0].vertices {
        assert!((v.normal[2] - 1.0).abs() < 1e-6, "{:?}", v.normal);
    }
}

#[test]
fn a_required_extension_it_lacks_is_refused_and_ignored_features_are_listed() {
    let (doc, bin) = triangle(
        json!({ "extensionsRequired": ["KHR_draco_mesh_compression"] }),
        true,
    );
    let err = load_glb(&doc, &bin).expect_err("refused");
    assert!(err.0.contains("KHR_draco_mesh_compression"), "{err:?}");
    let (doc, bin) = triangle(
        json!({ "animations": [{}], "skins": [{}], "extensionsUsed": ["EXT_lights_ies"] }),
        true,
    );
    let (_, ignored) = load_glb(&doc, &bin).expect("loads");
    assert_eq!(
        ignored.0.into_iter().collect::<Vec<_>>(),
        vec!["animations", "extensions", "skins"]
    );
}

#[test]
fn materials_and_textures_are_read() {
    let (mut doc, mut bin) = triangle(json!({}), true);
    let image_at = bin.len();
    bin.extend([7, 7, 7, 7]);
    let views = doc["bufferViews"].as_array_mut().expect("an array");
    views.push(json!({ "buffer": 0, "byteOffset": image_at, "byteLength": 4 }));
    let image_view = views.len() - 1;
    doc["buffers"][0]["byteLength"] = json!(bin.len());
    doc["images"] = json!([{ "bufferView": image_view, "mimeType": "image/png" }]);
    doc["textures"] = json!([{ "source": 0 }]);
    doc["materials"] = json!([{
        "name": "glass",
        "pbrMetallicRoughness": {
            "baseColorFactor": [1.0, 0.5, 0.25, 0.5],
            "baseColorTexture": { "index": 0 },
            "metallicFactor": 0.0,
            "roughnessFactor": 0.2
        },
        "emissiveFactor": [1.0, 0.0, 0.0],
        "alphaMode": "BLEND",
        "doubleSided": true,
        "extensions": {
            "KHR_materials_emissive_strength": { "emissiveStrength": 4.0 },
            "KHR_materials_unlit": {}
        }
    }]);
    doc["meshes"][0]["primitives"][0]["material"] = json!(0);
    let (model, _) = load_glb(&doc, &bin).expect("loads");
    let m = &model.materials()[0];
    assert_eq!(m.name, "glass");
    assert_eq!(m.base_color, [1.0, 0.5, 0.25, 0.5]);
    assert_eq!(m.alpha_mode, AlphaMode::Blend);
    assert_eq!(m.emissive, [4.0, 0.0, 0.0]);
    assert!(m.double_sided && m.unlit);
    assert_eq!(m.roughness, 0.2);
    let texture = m.base_color_texture.expect("a texture");
    assert_eq!(model.textures()[texture.texture].levels()[0], vec![7; 16]);
}

#[test]
fn an_accessor_past_its_view_is_refused() {
    let (mut doc, bin) = triangle(json!({}), true);
    doc["accessors"][0]["count"] = json!(30);
    let err = load_glb(&doc, &bin).expect_err("refused");
    assert!(err.0.contains("runs past"), "{err:?}");
}

#[test]
fn a_strip_becomes_triangles_that_face_the_same_way() {
    // A quad as a strip of four vertices: two triangles, both facing the viewer.
    let mut all = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0]);
    all.extend(f32s(&[0.0, 0.0, 1.0].repeat(4)));
    let doc = json!({
        "asset": { "version": "2.0" },
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0, "NORMAL": 1 }, "mode": 5 }] }],
        "accessors": [
            { "bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3" },
            { "bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3" }
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 48 },
            { "buffer": 0, "byteOffset": 48, "byteLength": 48 }
        ],
        "buffers": [{ "byteLength": all.len() }],
    });
    let (model, _) = load_glb(&doc, &all).expect("loads");
    assert_eq!(model.triangles(), 2);
    assert!(facing(&model).iter().all(|f| *f > 0.0));
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    let cases: [&[u8]; 4] = [
        b"",
        b"glTF",
        b"{\"nodes\": 3}",
        &[0x67, 0x6c, 0x54, 0x46, 2, 0, 0, 0, 255, 255, 255, 255],
    ];
    for bytes in cases {
        assert!(load(bytes, &mut Files::default()).is_err(), "{bytes:?}");
    }
}
