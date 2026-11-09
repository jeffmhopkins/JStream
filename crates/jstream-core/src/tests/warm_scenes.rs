//! Warm scenes (ADR-0117 §7): the captures of the Preview scene and of the scenes most
//! recently on program are published for the render thread to keep running beside the
//! program's, so showing one of them starts nothing.

use super::{add, batch, block_on, connected, create, edits, webcam};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{Action, Call, Edit, SceneId, SourceId, SourceKind};
use jstream_sources::{CaptureSpec, DeviceFormatWant};
use std::collections::BTreeSet;

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
