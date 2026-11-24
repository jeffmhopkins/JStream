//! Camera motion and follow cameras (ADR-0113): the shot redraws every frame while a
//! camera moves on its own, and `scene.zoom_camera` reaches the render thread on the
//! parameter channel; a Follow pointer filter supplies a 2D scene its camera (ADR-0120).

use super::{batch, block_on, code, connected, create, edits};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, Batch, Call, CameraFollow, CameraMotionModifier, CameraProperty, Canvas, Edit,
    Finite32, ItemGeometry, ItemId, MotionClock, MotionKind, SceneCamera, SceneId, Size, SourceId,
    SourceKind, Vec2,
};

fn zoom(scene: &str, steps: i32) -> Call {
    Call::Action {
        action: Action::SceneZoomCamera {
            scene_id: SceneId::new(scene),
            steps,
        },
    }
}

/// Scene `a`, 3D, with a display capture `screen` filling most of it, and scene `b`, 2D.
fn scenes() -> Vec<Edit> {
    let geometry = ItemGeometry::try_new(
        Vec2::new(960.0, 540.0).expect("finite"),
        Size::try_new(1440.0, 900.0).expect("size"),
    )
    .expect("geometry");
    vec![
        create("a"),
        create("b"),
        Edit::SourceCreate {
            source_id: SourceId::new("display"),
            name: "Display".into(),
            kind: SourceKind::Builtin {
                kind: "display".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
        },
        Edit::ItemAdd {
            scene_id: SceneId::new("a"),
            item_id: ItemId::new("screen"),
            source_id: SourceId::new("display"),
            transform: None,
            geometry: Some(geometry),
            opacity: None,
            blend: None,
        },
        Edit::SceneSetCamera {
            scene_id: SceneId::new("a"),
            camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
        },
    ]
}

#[test]
fn a_follow_camera_redraws_every_frame_and_takes_zoom_steps() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    assert!(!hub.load().shot().changes_with_time(), "a still camera");
    let failed = block_on(control.call(zoom("a", 1))).expect_err("no follow camera");
    assert_eq!(code(&failed), Some("action_failed"));

    batch(
        &control,
        Batch::single(Edit::SceneSetCameraFollow {
            scene_id: SceneId::new("a"),
            follow: Some(CameraFollow::of(ItemId::new("screen"))),
        }),
    )
    .expect("follow");
    assert!(hub.load().shot().changes_with_time(), "a follow camera");
    let family = SceneId::new("a");
    block_on(control.call(zoom("a", 2))).expect("zoom in");
    block_on(control.call(zoom("a", -1))).expect("zoom out");
    assert_eq!(hub.load_live().zooms(&family), 1);

    for (scene, steps, expected) in [
        ("a", 17, "invalid_argument"),
        ("b", 1, "action_failed"),
        ("nope", 1, "not_found"),
    ] {
        let failed = block_on(control.call(zoom(scene, steps))).expect_err(scene);
        assert_eq!(code(&failed), Some(expected), "{scene} {steps}");
    }
    assert_eq!(
        hub.load_live().zooms(&family),
        1,
        "a refused zoom takes no step"
    );
}

#[test]
fn camera_motion_redraws_every_frame() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    let sway = CameraMotionModifier {
        mode: jstream_protocol::MappingMode::Add,
        modifier_id: "sway".into(),
        property: CameraProperty::Roll,
        kind: MotionKind::Drift {
            rate_per_s: Finite32::ONE,
            wrap: None,
        },
        clock: MotionClock::Engine,
        enabled: true,
        amount: None,
    };
    let set = |motion: Vec<CameraMotionModifier>| {
        Batch::single(Edit::SceneSetCameraMotion {
            scene_id: SceneId::new("a"),
            motion,
        })
    };
    batch(&control, set(vec![sway.clone()])).expect("motion");
    assert!(hub.load().shot().changes_with_time(), "a swaying camera");
    let off = CameraMotionModifier {
        enabled: false,
        ..sway
    };
    batch(&control, set(vec![off])).expect("motion off");
    assert!(!hub.load().shot().changes_with_time(), "a disabled sway");
}

/// ADR-0176: a camera's `midi` modifier is followed by the shot's follower, by scene
/// family, from a named JTunnel sender on one channel.
#[test]
fn a_controller_drives_a_scene_camera() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    let f = |v: f32| Finite32::new(v).expect("finite");
    let zoom = CameraMotionModifier {
        mode: jstream_protocol::MappingMode::Add,
        modifier_id: "zoom".into(),
        property: CameraProperty::Fov,
        kind: MotionKind::Midi {
            control: jstream_protocol::MotionMidiControl::Cc,
            controller: Some(25),
            channel: Some(16),
            input: Some("Bitwig".into()),
            smoothing_ms: 0,
            curve: jstream_protocol::MotionCurve::Linear,
            from: f(0.0),
            to: f(-30.0),
        },
        clock: MotionClock::Engine,
        enabled: true,
        amount: None,
    };
    batch(
        &control,
        Batch::single(Edit::SceneSetCameraMotion {
            scene_id: SceneId::new("a"),
            motion: vec![zoom],
        }),
    )
    .expect("a MIDI camera move is taken");
    let snapshot = hub.load();
    let shot = snapshot.shot();
    assert!(shot.changes_with_time(), "a camera on a fader redraws");
    let family = SceneId::new("a");
    assert_eq!(
        shot.audio().camera_level(&family, "zoom"),
        Some(0.0),
        "nothing sent yet"
    );
    let midi = engine.midi_inputs();
    // Its MIDI may move things (ADR-0123 §3).
    midi.set_bindable(["Bitwig", "Other"].map(String::from));
    let cc = |channel: u8, value| {
        jstream_midi::Message::parse(&[0xB0 | channel, 25, value]).expect("a CC")
    };
    midi.push("Other", 0, &cc(15, 127));
    midi.push("Bitwig", 1, &cc(0, 127));
    let mut follower = crate::reactive::Follower::default();
    follower.follow(
        shot.audio(),
        jstream_media::MediaTime::new(0, jstream_media::Timebase::MICROS),
    );
    assert_eq!(
        shot.audio().camera_level(&family, "zoom"),
        Some(0.0),
        "another sender or channel moves nothing"
    );
    midi.push("Bitwig", 2, &cc(15, 127));
    follower.follow(
        shot.audio(),
        jstream_media::MediaTime::new(16_000, jstream_media::Timebase::MICROS),
    );
    assert_eq!(shot.audio().camera_level(&family, "zoom"), Some(1.0));
}

/// ADR-0176 decision 5: autofocus measures the item at the frame's centre, through the
/// camera as it is, and nothing when the camera looks away from it.
#[test]
fn autofocus_measures_the_item_at_the_centre() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut setup = scenes();
    setup.push(Edit::SourceCreate {
        source_id: SourceId::new("card"),
        name: "Card".into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: jstream_protocol::Settings(
            [
                ("color".to_owned(), serde_json::json!("#ff0000ff")),
                ("width".to_owned(), serde_json::json!(400)),
                ("height".to_owned(), serde_json::json!(300)),
            ]
            .into_iter()
            .collect(),
        ),
        duplicated_from: None,
    });
    setup.push(Edit::ItemAdd {
        scene_id: SceneId::new("a"),
        item_id: ItemId::new("card"),
        source_id: SourceId::new("card"),
        transform: None,
        geometry: Some(
            ItemGeometry::try_new(
                Vec2::new(960.0, 540.0).expect("finite"),
                Size::try_new(400.0, 300.0).expect("size"),
            )
            .expect("geometry"),
        ),
        opacity: None,
        blend: None,
    });
    batch(&control, edits(setup)).expect("scenes");
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("a"),
            preset_id: None,
            via: Vec::new(),
        },
    }))
    .expect("on program");
    assert!(engine.settle_sources(), "the sources were built in time");
    let framing = SceneCamera::framing(Canvas::DEFAULT);
    let snapshot = hub.load();
    let shot = snapshot.shot();
    let family = SceneId::new("a");
    let at = jstream_media::MediaTime::new(0, jstream_media::Timebase::MICROS);
    let met = shot
        .centre_distance(&family, framing, at)
        .expect("the screen is at the centre");
    assert!(
        (met - f64::from(framing.target_distance())).abs() < 0.5,
        "{met}"
    );
    let away = SceneCamera {
        target: jstream_protocol::Vec3::new(5000.0, 540.0, 0.0).expect("finite"),
        ..framing
    };
    assert_eq!(shot.centre_distance(&family, away, at), None);
    assert_eq!(shot.centre_distance(&SceneId::new("b"), framing, at), None);
    // With a video chain, the scene is drawn by a chain phase's pass, not the program's,
    // and autofocus measures it there.
    batch(
        &control,
        Batch::single(Edit::FilterAdd {
            owner: jstream_protocol::FilterOwner::Scene {
                scene_id: SceneId::new("a"),
            },
            chain: jstream_protocol::FilterChain::Video,
            filter_id: jstream_protocol::FilterId::new("look"),
            kind: "sharpen".into(),
            settings: Default::default(),
            enabled: None,
            index: None,
        }),
    )
    .expect("a scene chain");
    let snapshot = hub.load();
    let met = snapshot
        .shot()
        .centre_distance(&family, framing, at)
        .expect("the screen is at the centre, through the chain");
    assert!(
        (met - f64::from(framing.target_distance())).abs() < 0.5,
        "{met}"
    );
}

/// Autofocus focuses only on what hides what is behind it: a card in front of the screen
/// is met while it is a normal, opaque card, and passed over once it is blended onto
/// what is behind it (a glow or a hologram added on) or mostly faded, so showing or
/// hiding it doesn't pull the focus off the scene behind.
#[test]
fn autofocus_sees_through_blended_and_faded_items() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut setup = scenes();
    for (id, depth) in [("back", 0.0), ("glow", 500.0)] {
        setup.push(Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: id.into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: jstream_protocol::Settings(
                [
                    ("color".to_owned(), serde_json::json!("#ff0000ff")),
                    ("width".to_owned(), serde_json::json!(400)),
                    ("height".to_owned(), serde_json::json!(300)),
                ]
                .into_iter()
                .collect(),
            ),
            duplicated_from: None,
        });
        let mut geometry = ItemGeometry::try_new(
            Vec2::new(960.0, 540.0).expect("finite"),
            Size::try_new(400.0, 300.0).expect("size"),
        )
        .expect("geometry");
        geometry.depth_px = jstream_protocol::Finite32::new(depth).expect("finite");
        setup.push(Edit::ItemAdd {
            scene_id: SceneId::new("a"),
            item_id: ItemId::new(id),
            source_id: SourceId::new(id),
            transform: None,
            geometry: Some(geometry),
            opacity: None,
            blend: None,
        });
    }
    batch(&control, edits(setup)).expect("scenes");
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("a"),
            preset_id: None,
            via: Vec::new(),
        },
    }))
    .expect("on program");
    assert!(engine.settle_sources(), "the sources were built in time");
    let framing = SceneCamera::framing(Canvas::DEFAULT);
    let distance = f64::from(framing.target_distance());
    let family = SceneId::new("a");
    let at = jstream_media::MediaTime::new(0, jstream_media::Timebase::MICROS);
    let met = || {
        hub.load()
            .shot()
            .centre_distance(&family, framing, at)
            .expect("something is at the centre")
    };
    assert!(
        (met() - (distance - 500.0)).abs() < 0.5,
        "the card in front"
    );
    let glow = |edit: Edit| batch(&control, Batch::single(edit)).expect("an edit");
    glow(Edit::ItemSetBlend {
        item_id: ItemId::new("glow"),
        blend: jstream_protocol::BlendMode::Additive,
    });
    assert!(
        (met() - distance).abs() < 0.5,
        "added on, it is seen through"
    );
    glow(Edit::ItemSetBlend {
        item_id: ItemId::new("glow"),
        blend: jstream_protocol::BlendMode::Normal,
    });
    glow(Edit::ItemSetOpacity {
        item_id: ItemId::new("glow"),
        scene_id: None,
        opacity: jstream_protocol::Finite32::new(0.3).expect("finite"),
    });
    assert!(
        (met() - distance).abs() < 0.5,
        "mostly faded, it is seen through"
    );
    glow(Edit::ItemSetOpacity {
        item_id: ItemId::new("glow"),
        scene_id: None,
        opacity: jstream_protocol::Finite32::new(0.8).expect("finite"),
    });
    assert!(
        (met() - (distance - 500.0)).abs() < 0.5,
        "mostly opaque, it is met"
    );
}

/// ADR-0218: autofocus meets a model at the surface it draws, not at its item's flat box:
/// a cube 300 px deep is met at its front face, 150 px before its item's plane, from in
/// front and from behind; and from inside it, where only the backs of its faces are
/// seen, it isn't met.
#[test]
fn autofocus_meets_a_model_at_its_surface() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut setup = scenes();
    setup.push(Edit::SourceCreate {
        source_id: SourceId::new("cube"),
        name: "Cube".into(),
        kind: SourceKind::Builtin {
            kind: "model".into(),
        },
        settings: jstream_protocol::Settings(
            [("shape".to_owned(), serde_json::json!("cube"))]
                .into_iter()
                .collect(),
        ),
        duplicated_from: None,
    });
    setup.push(Edit::ItemAdd {
        scene_id: SceneId::new("a"),
        item_id: ItemId::new("cube"),
        source_id: SourceId::new("cube"),
        transform: None,
        geometry: Some(
            ItemGeometry::try_new(
                Vec2::new(960.0, 540.0).expect("finite"),
                Size::try_new(400.0, 300.0).expect("size"),
            )
            .expect("geometry"),
        ),
        opacity: None,
        blend: None,
    });
    batch(&control, edits(setup)).expect("scenes");
    block_on(control.call(Call::Action {
        action: Action::SceneSwitch {
            scene_id: SceneId::new("a"),
            preset_id: None,
            via: Vec::new(),
        },
    }))
    .expect("on program");
    assert!(engine.settle_sources(), "the sources were built in time");
    let framing = SceneCamera::framing(Canvas::DEFAULT);
    let snapshot = hub.load();
    let shot = snapshot.shot();
    let family = SceneId::new("a");
    let at = jstream_media::MediaTime::new(0, jstream_media::Timebase::MICROS);
    let distance = f64::from(framing.target_distance());
    // The first measure starts the cube's hierarchy building on a thread of its own;
    // until it is built, the cube isn't met and the screen behind it is.
    let start = std::time::Instant::now();
    // The cube fits the item's 300 px height, so its front face is 150 px nearer.
    let met = loop {
        let met = shot.centre_distance(&family, framing, at);
        if let Some(met) = met.filter(|met| *met < distance - 1.0) {
            break met;
        }
        assert!(
            met.is_some_and(|met| (met - distance).abs() < 0.5),
            "the screen, until the cube is built: {met:?}"
        );
        assert!(start.elapsed().as_secs() < 30, "the cube is at the centre");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!((met - (distance - 150.0)).abs() < 0.5, "{met}");
    let behind = SceneCamera {
        position: jstream_protocol::Vec3::new(960.0, 540.0, -1000.0).expect("finite"),
        target: jstream_protocol::Vec3::new(960.0, 540.0, 0.0).expect("finite"),
        ..framing
    };
    let met = shot
        .centre_distance(&family, behind, at)
        .expect("the cube's back is at the centre");
    assert!((met - 850.0).abs() < 0.5, "{met}");
    let inside = SceneCamera {
        position: jstream_protocol::Vec3::new(960.0, 540.0, 0.0).expect("finite"),
        target: jstream_protocol::Vec3::new(960.0, 540.0, -100.0).expect("finite"),
        ..framing
    };
    assert_eq!(shot.centre_distance(&family, inside, at), None);
    // Flipped, the cube draws its outward faces still, and is met at its front face.
    let mut flipped = ItemGeometry::try_new(
        Vec2::new(960.0, 540.0).expect("finite"),
        Size::try_new(400.0, 300.0).expect("size"),
    )
    .expect("geometry");
    flipped.flip_h = true;
    batch(
        &control,
        Batch::single(Edit::ItemSetGeometry {
            scene_id: None,
            item_id: ItemId::new("cube"),
            geometry: flipped,
        }),
    )
    .expect("flipped");
    let snapshot = hub.load();
    let met = snapshot
        .shot()
        .centre_distance(&family, framing, at)
        .expect("the flipped cube is at the centre");
    assert!((met - (distance - 150.0)).abs() < 0.5, "flipped: {met}");
}

/// ADR-0120: a Follow pointer filter on a capture in a 2D scene supplies the scene's
/// camera and moves it every frame; turned off, the scene is still and 2D again.
#[test]
fn a_follow_filter_moves_a_2d_scenes_camera_until_it_is_off() {
    use jstream_protocol::{FOLLOW_POINTER_KIND, FilterChain, FilterId, FilterOwner};
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut flat = scenes();
    flat.pop();
    batch(&control, edits(flat)).expect("scenes");
    assert!(!hub.load().shot().changes_with_time(), "a 2D scene");
    let owner = FilterOwner::Item {
        item_id: ItemId::new("screen"),
    };
    batch(
        &control,
        Batch::single(Edit::FilterAdd {
            owner: owner.clone(),
            chain: FilterChain::Video,
            filter_id: FilterId::new("follow"),
            kind: FOLLOW_POINTER_KIND.into(),
            settings: Default::default(),
            enabled: None,
            index: None,
        }),
    )
    .expect("the filter");
    assert!(hub.load().shot().changes_with_time(), "a follow camera");
    block_on(control.call(zoom("a", 1))).expect("the filter's scene zooms");
    batch(
        &control,
        Batch::single(Edit::FilterSetEnabled {
            owner,
            filter_id: FilterId::new("follow"),
            enabled: false,
        }),
    )
    .expect("off");
    assert!(!hub.load().shot().changes_with_time(), "off: still again");
    let failed = block_on(control.call(zoom("a", 1))).expect_err("no follow camera");
    assert_eq!(code(&failed), Some("action_failed"));
}

fn drive(scene: &str, pan: Option<i8>, tilt: Option<i8>, zoom: Option<i8>) -> Call {
    Call::Action {
        action: Action::SceneDriveCamera {
            scene_id: SceneId::new(scene),
            pan,
            tilt,
            zoom,
        },
    }
}

fn aim_at(scene: &str, pan: f32, tilt: f32, zoom: f32) -> Call {
    Call::Action {
        action: Action::SceneAimCamera {
            scene_id: SceneId::new(scene),
            aim: jstream_protocol::CameraAim {
                pan_deg: Finite32::new(pan),
                tilt_deg: Finite32::new(tilt),
                zoom: Finite32::new(zoom),
            },
            speed: None,
        },
    }
}

fn camera_aim(control: &dyn Control, scene: &str) -> jstream_protocol::CameraAimInfo {
    match super::query(
        control,
        jstream_protocol::Query::SceneCameraAim {
            scene_id: SceneId::new(scene),
        },
    )
    .expect("scene.camera_aim")
    {
        jstream_protocol::QueryResult::SceneCameraAim { aim } => aim,
        other => panic!("{other:?}"),
    }
}

/// ADR-0152: `scene.drive_camera` and `scene.aim_camera` move a scene family's live aim,
/// which reaches the render thread on the parameter channel and `scene.camera_aim`
/// reads; a refused call moves nothing, and a drive stops when its session ends.
#[test]
fn a_scene_cameras_live_aim_is_driven_aimed_and_read() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    batch(&control, edits(scenes())).expect("scenes");
    batch(
        &control,
        Batch::single(Edit::SceneCreateVariant {
            scene_id: SceneId::new("a2"),
            variant_of: SceneId::new("a"),
            name: "A2".into(),
        }),
    )
    .expect("keyframe");

    let home = camera_aim(&control, "a");
    assert_eq!(
        (
            home.pan_deg.get(),
            home.tilt_deg.get(),
            home.zoom.get(),
            home.moving
        ),
        (0.0, 0.0, 1.0, false),
        "a camera nobody aimed is at its home"
    );
    assert!(!hub.load_live().aims_move());

    // An aim through a keyframe scene aims its family's camera, at once at the fastest
    // speed for a small turn.
    block_on(control.call(aim_at("a2", 1.0, -1.0, 1.0))).expect("aim");
    assert!(
        hub.load_live().aims_move(),
        "the aim reached the render thread"
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let aimed = camera_aim(&control, "a");
    assert_eq!(
        (aimed.pan_deg.get(), aimed.tilt_deg.get(), aimed.zoom.get()),
        (1.0, -1.0, 1.0)
    );
    assert_eq!(camera_aim(&control, "a2"), aimed, "one aim for the family");
    // A zoom to 2 at the fastest speed glides for most of a second.
    block_on(control.call(aim_at("a", 1.0, -1.0, 2.0))).expect("zoom");
    let zooming = camera_aim(&control, "a");
    assert!(zooming.moving && zooming.zoom.get() < 2.0, "{zooming:?}");
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(camera_aim(&control, "a").zoom.get(), 2.0, "there");

    // A drive moves it until a stop.
    block_on(control.call(drive("a", Some(24), None, None))).expect("drive");
    assert!(camera_aim(&control, "a").moving);
    std::thread::sleep(std::time::Duration::from_millis(20));
    block_on(control.call(drive("a", Some(0), None, None))).expect("stop");
    let stopped = camera_aim(&control, "a");
    assert!(
        !stopped.moving && stopped.pan_deg.get() > 1.0,
        "{stopped:?}"
    );

    for (call, expected) in [
        (drive("a", Some(25), None, None), "invalid_argument"),
        (drive("a", None, None, None), "invalid_argument"),
        (aim_at("a", 171.0, 0.0, 1.0), "invalid_argument"),
        (aim_at("a", 0.0, 0.0, 9.0), "invalid_argument"),
        (drive("b", Some(1), None, None), "action_failed"),
        (aim_at("b", 0.0, 0.0, 1.0), "action_failed"),
        (drive("nope", Some(1), None, None), "not_found"),
    ] {
        let failed = block_on(control.call(call.clone())).expect_err("refused");
        assert_eq!(code(&failed), Some(expected), "{call:?}");
    }
    assert_eq!(
        camera_aim(&control, "a"),
        stopped,
        "a refused call moves nothing"
    );

    // `scene.drive_camera` can't be bound; `scene.aim_camera` can.
    let bind = |action: Action| {
        batch(
            &control,
            Batch::single(Edit::HotkeySet {
                chord: jstream_protocol::Chord::new("F1"),
                action,
            }),
        )
    };
    let refused = bind(Action::SceneDriveCamera {
        scene_id: SceneId::new("a"),
        pan: Some(4),
        tilt: None,
        zoom: None,
    })
    .expect_err("a drive can't be bound");
    assert_eq!(code(&refused), Some("invalid_argument"));

    // A drive belongs to its session: when that session ends, the camera stops.
    let other = connected(&engine);
    block_on(other.call(drive("a", None, Some(-23), None))).expect("drive");
    assert!(camera_aim(&control, "a").moving);
    drop(other);
    let left = camera_aim(&control, "a");
    assert!(!left.moving, "{left:?}");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(
        camera_aim(&control, "a"),
        left,
        "it stays where the drive left it"
    );
}
