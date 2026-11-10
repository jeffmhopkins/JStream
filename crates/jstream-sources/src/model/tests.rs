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
            &err,
            SourceError::Model { reason, .. } if reason.contains("`..`")
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

/// A `.gltf` of one triangle on a node that a clip `slide` moves 4 along x over 2 s.
fn write_sliding(dir: &std::path::Path) {
    let bin: Vec<u8> = [
        0.0f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0, // positions
        0.0, 2.0, // times
        0.0, 0.0, 0.0, 4.0, 0.0, 0.0, // translations
    ]
    .iter()
    .flat_map(|v| v.to_le_bytes())
    .collect();
    let doc = json!({
        "asset": { "version": "2.0" },
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [
            { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3" },
            { "bufferView": 1, "componentType": 5126, "count": 2, "type": "SCALAR" },
            { "bufferView": 2, "componentType": 5126, "count": 2, "type": "VEC3" }
        ],
        "bufferViews": [
            { "buffer": 0, "byteLength": 36 },
            { "buffer": 0, "byteOffset": 36, "byteLength": 8 },
            { "buffer": 0, "byteOffset": 44, "byteLength": 24 }
        ],
        "buffers": [{ "uri": "slide.bin", "byteLength": 68 }],
        "animations": [{
            "name": "slide",
            "samplers": [{ "input": 1, "output": 2 }],
            "channels": [{ "sampler": 0, "target": { "node": 0, "path": "translation" } }]
        }]
    });
    std::fs::write(
        dir.join("set/slide.gltf"),
        serde_json::to_vec(&doc).expect("json"),
    )
    .expect("written");
    std::fs::write(dir.join("set/slide.bin"), bin).expect("written");
}

#[test]
fn an_animation_plays_on_the_media_clock_loops_or_holds_and_a_wrong_name_is_refused() {
    use jstream_media::{MediaTime, Timebase};
    let temp = folder("slide");
    write_sliding(&temp.0);
    let ctx = Context {
        assets: AssetRoots {
            collection: Some(temp.0.clone()),
            app: None,
        },
        ..Context::default()
    };
    let ms = |t: i64| MediaTime::new(t, Timebase::MILLIS);
    // Where the clip has slid the node at `t` ms: its slot's translation along x.
    let slid = |source: &mut Box<dyn Source>, t: i64| {
        source.frame(ms(t)).expect("a frame");
        source.model().expect("a model").pose.expect("posed")[0][12]
    };
    let mut source = create(
        json!({ "file": file("set/slide.gltf"), "animation": "slide" }),
        &ctx,
    )
    .expect("loads");
    assert!((slid(&mut source, 1000) - 2.0).abs() < 1e-5);
    // Looped: 3 s is 1 s into the second pass.
    assert!((slid(&mut source, 3000) - 2.0).abs() < 1e-5);
    assert!(source.next_change(ms(3000)).is_some());
    // Twice as fast, held at its end.
    source
        .update(&settings(json!({
            "file": file("set/slide.gltf"),
            "animation": "slide",
            "animation_speed": 2.0,
            "animation_loop": false,
        })))
        .expect("live");
    assert!((slid(&mut source, 500) - 2.0).abs() < 1e-5);
    assert!((slid(&mut source, 5000) - 4.0).abs() < 1e-5);
    assert_eq!(source.next_change(ms(5000)), None);
    // No animation stands it at rest.
    let mut still = create(json!({ "file": file("set/slide.gltf") }), &ctx).expect("loads");
    still.frame(ms(1000)).expect("a frame");
    assert!(still.model().expect("a model").pose.is_none());
    // A name the file lacks is refused with the names it has.
    let err = create(
        json!({ "file": file("set/slide.gltf"), "animation": "walk" }),
        &ctx,
    )
    .err()
    .expect("refused");
    assert!(err.to_string().contains("it has slide"), "{err}");
}
