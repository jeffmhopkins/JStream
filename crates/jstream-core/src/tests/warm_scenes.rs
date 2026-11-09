//! Warm scenes (ADR-0117 §7): the captures of the Preview scene and of the scenes most
//! recently on program are published for the render thread to keep running beside the
//! program's, and their other sources never rest, so showing one of them starts nothing.

use super::{add, batch, block_on, connected, create, edits, webcam};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{Action, Call, Edit, SceneId, SourceId, SourceKind};
use jstream_sources::{CaptureSpec, DeviceFormatWant};
use serde_json::json;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn screen() -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new("screen"),
        name: "Screen".into(),
        kind: SourceKind::Builtin {
            kind: "display".into(),
        },
        settings: Default::default(),
        duplicated_from: None,
    }
}

fn act(control: &dyn Control, action: Action) {
    block_on(control.call(Call::Action { action })).expect("the action applies");
}

fn switch(control: &dyn Control, scene: &str) {
    act(
        control,
        Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: None,
        },
    );
}

fn preview(control: &dyn Control, scene: &str) {
    act(
        control,
        Action::StudioSetPreview {
            scene_id: SceneId::new(scene),
        },
    );
}

#[test]
fn the_preview_and_recent_scenes_keep_their_captures_warm() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let scenes = vec![
        create("main"),
        create("b"),
        screen(),
        add("b", "shown", "screen"),
        webcam(),
        add("b", "cam", "webcam"),
    ];
    batch(&control, edits(scenes)).expect("scenes");
    assert!(engine.settle_sources(), "the capture sources were built");
    let warm = || -> BTreeSet<CaptureSpec> {
        hub.load_warm().iter().map(|key| key.spec.clone()).collect()
    };
    let b: BTreeSet<CaptureSpec> = [
        CaptureSpec::Display(None),
        CaptureSpec::Device(None, DeviceFormatWant::default()),
    ]
    .into();
    assert_eq!(warm(), BTreeSet::new(), "b is neither preview nor shown");

    preview(&control, "b");
    assert_eq!(warm(), b, "the Preview scene is warm");

    switch(&control, "b");
    assert_eq!(
        warm(),
        BTreeSet::new(),
        "the program's captures are its own"
    );
    assert_eq!(hub.load().shot().captures().len(), 2);

    preview(&control, "main");
    switch(&control, "main");
    assert_eq!(warm(), b, "the scene shown last stays warm");

    batch(
        &control,
        edits(vec![Edit::SceneDelete {
            scene_id: SceneId::new("b"),
        }]),
    )
    .expect("deleted");
    assert_eq!(warm(), BTreeSet::new(), "a deleted scene is not warm");
}

/// A waveform on scene `scene`, painted on the live sources' thread.
fn waveform(id: &str) -> Edit {
    let settings = [
        ("width", json!(1)),
        ("height", json!(3)),
        ("color", json!("#ff0000")),
        ("background", json!("#0000ff")),
    ];
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "waveform".into(),
        },
        settings: jstream_protocol::Settings(
            settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        ),
        duplicated_from: None,
    }
}

/// Whether `engine` paints `source` at least `more` more times within 5 s, with sound
/// keeping the waveforms moving.
fn painted(engine: &Engine, source: &str, more: u64) -> bool {
    let scope = engine.audio_scope();
    let before = engine.paints(source);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if engine.paints(source) >= before + more {
            return true;
        }
        scope.push(48_000, &[1.0; 960]);
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// A warm scene's source keeps being painted though no mix draws it, and rests once
/// its scene is no longer warm.
#[test]
fn a_warm_scenes_sources_never_rest() {
    let engine = Engine::new();
    let control = connected(&engine);
    let scenes = vec![
        create("a"),
        waveform("on"),
        add("a", "i1", "on"),
        create("b"),
        waveform("off"),
        add("b", "i2", "off"),
    ];
    batch(&control, edits(scenes)).expect("created");
    assert!(engine.settle_sources(), "built");
    assert!(
        painted(&engine, "on", 10),
        "the program's waveform is painted"
    );
    assert_eq!(
        engine.resting("off"),
        Some(true),
        "b is neither drawn nor warm"
    );

    preview(&control, "b");
    assert_eq!(
        engine.resting("off"),
        Some(false),
        "the Preview scene is warm"
    );
    assert!(painted(&engine, "off", 5), "and its waveform is painted");

    preview(&control, "a");
    assert_eq!(engine.resting("off"), Some(true), "b is no longer warm");

    switch(&control, "b");
    switch(&control, "a");
    assert_eq!(
        engine.resting("off"),
        Some(false),
        "the scene shown last stays warm"
    );
}
