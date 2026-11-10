//! PTZ cameras through the engine (ADR-0146): `ptz.*` actions and `ptz.list` reach an
//! emulated head, and scenes fire their shots when they go on program or preview.

use super::{add, batch, block_on, code, connected, create, edits, query};
use crate::Engine;
use crate::ptz::tests::{FakeHead, HeadState};
use crate::ptz::{PtzSetup, matched_speed};
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, Easing, Edit, PtzCameraProfile, PtzPose, PtzShot, PtzTransport, Query,
    QueryResult, SceneId, Settings, SourceId, SourceKind, TransitionPresetId,
};
use jstream_visca::{Command, PositionWidth, PresetOp};

fn call(control: &dyn Control, action: Action) -> Result<(), String> {
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|e| code(&e).unwrap_or("other").to_owned())
}

fn shot(camera: &str, preset: Option<u8>, pan: Option<i32>) -> PtzShot {
    PtzShot {
        camera: camera.into(),
        preset,
        pose: PtzPose {
            pan,
            tilt: pan.map(|_| 0),
            ..PtzPose::default()
        },
    }
}

fn shots(scene: &str, shots: Vec<PtzShot>) -> Edit {
    Edit::SceneSetPtzShots {
        scene_id: SceneId::new(scene),
        shots,
    }
}

fn switch(control: &dyn Control, scene: &str, preset: Option<&str>) {
    let action = Action::SceneSwitch {
        scene_id: SceneId::new(scene),
        preset_id: preset.map(TransitionPresetId::new),
    };
    call(control, action).expect("scene.switch");
}

fn preview(control: &dyn Control, scene: &str) {
    let action = Action::StudioSetPreview {
        scene_id: SceneId::new(scene),
    };
    call(control, action).expect("studio.set_preview");
}

/// `profile.get`'s `ptz_cameras` (ADR-0155).
fn ptz_profile(control: &dyn Control) -> Vec<PtzCameraProfile> {
    match query(
        control,
        Query::ProfileGet {
            seconds: None,
            top: None,
        },
    ) {
        Ok(QueryResult::ProfileGet { profile }) => profile.ptz_cameras,
        other => panic!("{other:?}"),
    }
}

fn recalled(head: &HeadState) -> Vec<u8> {
    head.commands
        .iter()
        .filter_map(|c| match c {
            Command::Preset(PresetOp::Recall, p) => Some(*p),
            _ => None,
        })
        .collect()
}

fn aims(head: &HeadState) -> Vec<(i32, u8)> {
    head.commands
        .iter()
        .filter_map(|c| match c {
            Command::PanTiltAbsolute { pan, pan_speed, .. } => Some((*pan, *pan_speed)),
            _ => None,
        })
        .collect()
}

/// Without `start_ptz` the engine controls no camera: an action fails and the list is
/// empty. With it, actions reach the head, `ptz.list` shows its pose and `profile.get`
/// counts its link (ADR-0155).
#[test]
fn actions_and_the_list_reach_the_heads_the_host_starts() {
    let engine = Engine::new();
    let control = connected(&engine);
    let home = || Action::PtzHome {
        camera: "wide".into(),
    };
    assert_eq!(call(&control, home()), Err("action_failed".into()));
    assert_eq!(
        query(&control, Query::PtzList).ok(),
        Some(QueryResult::PtzList {
            cameras: Vec::new()
        })
    );
    assert_eq!(ptz_profile(&control), Vec::new());
    let head = FakeHead::start(PtzTransport::ViscaIp);
    engine
        .start_ptz(&PtzSetup::new(vec![head.camera("wide")]).expect("setup"))
        .expect("start");
    assert_eq!(
        call(
            &control,
            Action::PtzAim {
                camera: "wide".into(),
                pose: PtzPose {
                    pan: Some(500),
                    tilt: Some(-20),
                    ..PtzPose::default()
                },
                speed: None,
            }
        ),
        Ok(())
    );
    assert!(
        head.wait(|h| (h.pan, h.tilt) == (500, -20)),
        "{:?}",
        head.state()
    );
    assert_eq!(
        call(
            &control,
            Action::PtzHome {
                camera: "tele".into()
            }
        ),
        Err("invalid_argument".into())
    );
    let mut listed = None;
    for _ in 0..100 {
        if let Ok(QueryResult::PtzList { cameras }) = query(&control, Query::PtzList)
            && cameras[0].pose.is_some_and(|p| p.pan == Some(500))
        {
            listed = Some(cameras);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    let cameras = listed.expect("the pose reaches ptz.list");
    assert_eq!(cameras[0].camera, "wide");
    assert!(cameras[0].reachable);
    let profiled = ptz_profile(&control);
    assert_eq!(profiled.len(), 1, "{profiled:?}");
    let wide = &profiled[0];
    assert_eq!(wide.camera, "wide");
    assert_eq!(wide.address, head.addr.to_string());
    assert_eq!(wide.transport, "visca_ip");
    assert!(wide.reachable);
    assert!(wide.sent > 0 && wide.replies > 0, "{wide:?}");
    // The head answered every command, the first time.
    assert_eq!(
        (
            wide.dropped,
            wide.resent,
            wide.unanswered,
            wide.refused,
            wide.busy
        ),
        (0, 0, 0, 0, 0)
    );
    // An empty setup stops them, and they leave the report.
    engine.start_ptz(&PtzSetup::default()).expect("stop");
    assert_eq!(call(&control, home()), Err("action_failed".into()));
    assert_eq!(ptz_profile(&control), Vec::new());
}

/// A scene going on program sends its shots; a keyframe scene sends its own shot for a
/// camera and its parent's for the others; a shot for a camera this machine doesn't
/// list is skipped (ADR-0146 §6, §7). A longer transition paces a pose more slowly.
#[test]
fn program_switches_fire_the_scenes_shots() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let wide = FakeHead::start(PtzTransport::ViscaIp);
    let close = FakeHead::start(PtzTransport::Udp);
    engine
        .start_ptz(&PtzSetup::new(vec![wide.camera("wide"), close.camera("close")]).expect("setup"))
        .expect("start");
    batch(
        &control,
        edits(vec![
            create("a"),
            create("b"),
            // Something on screen, so the switch to `b` runs its transition.
            Edit::SourceCreate {
                source_id: SourceId::new("red"),
                name: "red".into(),
                kind: SourceKind::Builtin {
                    kind: "color".into(),
                },
                settings: Settings(
                    [("color".to_owned(), serde_json::json!("#ff0000ff"))]
                        .into_iter()
                        .collect(),
                ),
                duplicated_from: None,
            },
            add("a", "a1", "red"),
            add("b", "b1", "red"),
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("a_close"),
                variant_of: SceneId::new("a"),
                name: "A close".into(),
            },
            shots(
                "a",
                vec![
                    shot("wide", Some(1), None),
                    shot("close", None, Some(-900)),
                    shot("absent", Some(9), None),
                ],
            ),
            shots("a_close", vec![shot("close", Some(7), None)]),
            shots("b", vec![shot("wide", None, Some(4000))]),
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("slow"),
                name: "Slow".into(),
                kind: "fade".into(),
                duration_ms: 2000,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
        ]),
    )
    .expect("batch");
    assert!(engine.settle_sources(), "the source was built in time");
    switch(&control, "a", None);
    assert!(wide.wait(|h| recalled(h) == [1]), "{:?}", wide.state());
    assert!(
        close.wait(|h| aims(h) == [(-900, 24)]),
        "{:?}",
        close.state()
    );
    // The keyframe scene recalls its own preset on `close`, and `a`'s on `wide`.
    switch(&control, "a_close", None);
    assert!(close.wait(|h| recalled(h) == [7]), "{:?}", close.state());
    assert!(wide.wait(|h| recalled(h) == [1, 1]), "{:?}", wide.state());
    // A two-second transition paces `b`'s pose.
    switch(&control, "b", Some("slow"));
    let paced = matched_speed(2000);
    assert!(paced < 24);
    assert!(
        wide.wait(|h| aims(h) == [(4000, paced)]),
        "{:?}",
        wide.state()
    );
    let state = wide.state();
    assert!(state.commands.contains(&Command::PanTiltAbsolute {
        pan_speed: paced,
        tilt_speed: paced,
        pan: 4000,
        tilt: 0,
        width: PositionWidth::Pan5Tilt4,
    }));
    // Switching to the scene already on program fires nothing.
    switch(&control, "b", None);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(aims(&wide.state()).len(), 1);
}

/// A scene going on preview moves only the cameras the program's shots don't name, so
/// a preview never moves a live shot (ADR-0146 §7). Editing shots moves nothing.
#[test]
fn preview_never_moves_a_camera_the_program_frames() {
    let engine = Engine::new();
    let control = connected(&engine);
    let wide = FakeHead::start(PtzTransport::ViscaIp);
    let close = FakeHead::start(PtzTransport::ViscaIp);
    engine
        .start_ptz(&PtzSetup::new(vec![wide.camera("wide"), close.camera("close")]).expect("setup"))
        .expect("start");
    batch(
        &control,
        edits(vec![
            create("live"),
            create("next"),
            shots("live", vec![shot("wide", Some(1), None)]),
            shots(
                "next",
                vec![shot("wide", Some(2), None), shot("close", Some(3), None)],
            ),
        ]),
    )
    .expect("batch");
    switch(&control, "live", None);
    assert!(wide.wait(|h| recalled(h) == [1]));
    preview(&control, "next");
    assert!(close.wait(|h| recalled(h) == [3]), "{:?}", close.state());
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(recalled(&wide.state()), [1], "the live camera stays");
    // An edit to the program scene's shots doesn't move its head.
    batch(
        &control,
        edits(vec![shots("live", vec![shot("wide", Some(5), None)])]),
    )
    .expect("batch");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(recalled(&wide.state()), [1]);
    // Taking `next` to program moves both.
    switch(&control, "next", None);
    assert!(wide.wait(|h| recalled(h) == [1, 2]), "{:?}", wide.state());
    assert!(close.wait(|h| recalled(h) == [3, 3]), "{:?}", close.state());
}
