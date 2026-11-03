//! The `model` kind: settings, live looks, the stand-in frame and reading a file through
//! the asset roots.

use std::path::PathBuf;

use jstream_media::model::Lighting;
use jstream_protocol::{AssetRoot, Reference, Settings};
use serde_json::{Value, json};

use super::{ModelKind, NATURAL_SIDE};
use crate::{AssetRoots, Context, Source, SourceError, SourceKind};

fn settings(overrides: Value) -> Settings {
    let mut settings: Settings = ModelKind::new()
        .descriptor()
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    if let Value::Object(map) = overrides {
        for (k, v) in map {
            settings.insert(k, v);
        }
    }
    settings
}

fn create(overrides: Value, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
    ModelKind::new().create(&settings(overrides), ctx)
}

#[test]
fn a_shape_is_a_model_with_a_square_stand_in() {
    let mut source = create(json!({ "shape": "sphere" }), &Context::default()).expect("a sphere");
    let shown = source.model().expect("a model");
    assert_eq!(shown.lighting, Lighting::Studio);
    assert_eq!(shown.model.materials()[0].roughness, 0.5);
    let frame = source
        .frame(jstream_media::MediaTime::zero(
            jstream_media::Timebase::MILLIS,
        ))
        .expect("a frame");
    assert_eq!(
        (frame.info().size.width(), frame.info().size.height()),
        (NATURAL_SIDE, NATURAL_SIDE)
    );
}

#[test]
fn no_file_is_no_model() {
    let source = create(json!({}), &Context::default()).expect("no file yet");
    assert!(source.model().is_none());
}

#[test]
fn the_look_is_live_and_the_shape_is_not() {
    let mut source = create(json!({ "shape": "cube" }), &Context::default()).expect("a cube");
    let before = source.model().expect("a model");
    source
        .update(&settings(json!({
            "shape": "cube",
            "color": "#ff0000",
            "metallic": 1.0,
            "roughness": 0.25,
            "lighting": "unlit",
            "double_sided": true,
        })))
        .expect("live");
    let after = source.model().expect("a model");
    assert_ne!(before.model.version(), after.model.version());
    let m = &after.model.materials()[0];
    assert_eq!(m.base_color, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!((m.metallic, m.roughness, m.double_sided), (1.0, 0.25, true));
    assert_eq!(after.lighting, Lighting::Unlit);
    // The same settings again are the same model: nothing to upload.
    source
        .update(&settings(json!({
            "shape": "cube",
            "color": "#ff0000",
            "metallic": 1.0,
            "roughness": 0.25,
            "lighting": "unlit",
            "double_sided": true,
        })))
        .expect("live");
    assert_eq!(
        source.model().expect("a model").model.version(),
        after.model.version()
    );
    let err = source
        .update(&settings(json!({ "shape": "sphere" })))
        .expect_err("not live");
    assert!(matches!(err, SourceError::NotLive { param } if param == "shape"));
}

#[test]
fn bad_settings_are_refused() {
    for bad in [
        json!({ "shape": "teapot" }),
        json!({ "shape": "cube", "roughness": 2.0 }),
        json!({ "shape": "cube", "lighting": "noir" }),
        json!({ "shape": "cube", "color": "red" }),
    ] {
        assert!(
            matches!(
                create(bad.clone(), &Context::default()),
                Err(SourceError::Setting { .. })
            ),
            "{bad}"
        );
    }
}

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn folder(name: &str) -> Temp {
    let dir = std::env::temp_dir().join(format!("jstream-model-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("set")).expect("a temp folder");
    Temp(dir)
}

/// A `.gltf` of one triangle whose buffer is a file beside it.
fn write_triangle(dir: &std::path::Path, buffer_uri: &str) {
    let mut bin: Vec<u8> = [0.0f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    bin.extend([0u8; 0]);
    let doc = json!({
        "asset": { "version": "2.0" },
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3" }],
        "bufferViews": [{ "buffer": 0, "byteLength": 36 }],
        "buffers": [{ "uri": buffer_uri, "byteLength": 36 }],
    });
    std::fs::write(
        dir.join("set/tri.gltf"),
        serde_json::to_vec(&doc).expect("json"),
    )
    .expect("written");
    std::fs::write(dir.join("set/tri.bin"), bin).expect("written");
}

fn file(path: &str) -> Value {
    Reference::Asset {
        root: AssetRoot::Collection,
        path: path.into(),
    }
    .to_value()
}

#[test]
fn a_gltf_reads_its_buffer_beside_it_under_the_same_root() {
    let temp = folder("beside");
    write_triangle(&temp.0, "tri.bin");
    let ctx = Context {
        assets: AssetRoots {
            collection: Some(temp.0.clone()),
            app: None,
        },
        ..Context::default()
    };
    let mut source = create(json!({ "file": file("set/tri.gltf") }), &ctx).expect("loads");
    let shown = source.model().expect("a model");
    assert_eq!(shown.model.triangles(), 1);
    // Twice as wide as it is tall: 512 by 256.
    let frame = source
        .frame(jstream_media::MediaTime::zero(
            jstream_media::Timebase::MILLIS,
        ))
        .expect("a frame");
    assert_eq!(
        (frame.info().size.width(), frame.info().size.height()),
        (NATURAL_SIDE, NATURAL_SIDE / 2)
    );
}

#[test]
fn a_gltf_cant_reach_outside_its_root() {
    let temp = folder("escape");
    write_triangle(&temp.0, "../../outside.bin");
    let ctx = Context {
        assets: AssetRoots {
            collection: Some(temp.0.clone()),
            app: None,
        },
        ..Context::default()
    };
    let Err(err) = create(json!({ "file": file("set/tri.gltf") }), &ctx) else {
        panic!("refused")
    };
    assert!(
        matches!(
            err,
            SourceError::AssetEscapesRoot { .. } | SourceError::AssetRead { .. }
        ),
        "{err:?}"
    );
}

#[test]
fn a_file_that_isnt_a_model_names_itself() {
    let temp = folder("garbage");
    std::fs::write(temp.0.join("set/bad.glb"), b"not a model").expect("written");
    let ctx = Context {
        assets: AssetRoots {
            collection: Some(temp.0.clone()),
            app: None,
        },
        ..Context::default()
    };
    let Err(err) = create(json!({ "file": file("set/bad.glb") }), &ctx) else {
        panic!("refused")
    };
    assert!(
        matches!(&err, SourceError::Model { path, .. } if path == "set/bad.glb"),
        "{err:?}"
    );
}
