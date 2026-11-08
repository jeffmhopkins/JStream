//! Frame sheets (#1061): `video.frames` and `scene.switch_and_watch` take their pictures
//! in real time, tile them in order, and say when each was taken.

use super::{add, batch, block_on, connected, create, edits, query};
use crate::Engine;
use crate::snapshot::{decode_base64, decode_png, is_no_adapter};
use jstream_control::{Control, ControlError};
use jstream_protocol::{
    Action, ActionResult, Call, CallResult, Easing, Edit, Finite32, FrameSheet, FrameStrip, ItemId,
    MotionClock, MotionCurve, MotionKind, MotionMeasure, MotionModifier, MotionProperty,
    ProtocolError, Query, QueryResult, SceneId, Settings, SourceId, SourceKind, TransitionPresetId,
};
use serde_json::json;
use std::io::Write;

fn fill(id: &str, hex: &str) -> Edit {
    let settings = [
        ("color", json!(hex)),
        ("width", json!(1920)),
        ("height", json!(1080)),
    ];
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "color".into(),
        },
        settings: Settings(
            settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        ),
        duplicated_from: None,
    }
}

/// Scene `a` red on program, scene `b` green, and a linear 600 ms fade.
fn two_scenes(engine: &Engine, control: &dyn Control) {
    batch(
        control,
        edits(vec![
            create("a"),
            create("b"),
            fill("red", "#ff0000ff"),
            fill("green", "#00ff00ff"),
            add("a", "a1", "red"),
            add("b", "b1", "green"),
            Edit::TransitionCreate {
                preset_id: TransitionPresetId::new("fade"),
                name: "fade".into(),
                kind: "fade".into(),
                duration_ms: 600,
                easing: Easing::Linear,
                settings: Settings::default(),
            },
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
}

/// The sheet, or `None` (skipping the test) without an adapter.
fn sheet_or_skip(result: Result<FrameSheet, ControlError>) -> Option<FrameSheet> {
    match result {
        Ok(sheet) => Some(sheet),
        Err(error) if error.protocol().is_some_and(is_no_adapter) => {
            if std::env::var_os("CI").is_some() {
                panic!("CI has no wgpu adapter: {error}");
            }
            let _ = writeln!(
                std::io::stderr(),
                "jstream-core: skipping frame sheet GPU test, no adapter: {error}"
            );
            None
        }
        Err(error) => panic!("the frame sheet failed: {error}"),
    }
}

fn frames(
    control: &dyn Control,
    strip: FrameStrip,
    scene: Option<&str>,
) -> Result<FrameSheet, ControlError> {
    let result = query(
        control,
        Query::VideoFrames {
            strip,
            scene_id: scene.map(SceneId::new),
        },
    )?;
    match result {
        QueryResult::VideoFrames { sheet } => Ok(sheet),
        other => panic!("not a frame sheet: {other:?}"),
    }
}

fn switch_and_watch(
    control: &dyn Control,
    scene: &str,
    preset: Option<&str>,
    strip: FrameStrip,
) -> Result<FrameSheet, ControlError> {
    let action = Action::SceneSwitchAndWatch {
        scene_id: SceneId::new(scene),
        preset_id: preset.map(TransitionPresetId::new),
        strip,
    };
    match block_on(control.call(Call::Action { action }))?.result {
        CallResult::Action {
            result: Some(ActionResult::SceneSwitchAndWatch { sheet }),
        } => Ok(sheet),
        other => panic!("not a frame sheet: {other:?}"),
    }
}

/// The centre pixel of each tile, in order.
fn centres(sheet: &FrameSheet) -> Vec<[u8; 4]> {
    let bytes = decode_base64(&sheet.png).expect("png is base64");
    let (width, height, rgba) = decode_png(&bytes).expect("png decodes");
    assert_eq!((width, height), (sheet.width, sheet.height));
    (0..sheet.frames.len() as u32)
        .map(|index| {
            let (column, row) = (index % sheet.columns, index / sheet.columns);
            let x = 4 + column * (sheet.tile_width + 4) + sheet.tile_width / 2;
            let y = 4 + row * (sheet.tile_height + 4) + sheet.tile_height / 2;
            let i = ((y * width + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        })
        .collect()
}

/// The switch's fade shows in order: the first tile is still mostly red, the ones the
/// fade is half-way through mix red and green, the ones after its end are all green,
/// and green only grows from tile to tile. Each tile is judged by the time it says it
/// was taken, so a slow machine that takes a picture late still passes.
#[test]
fn a_switch_and_watch_shows_the_fade_as_it_plays() {
    watch_the_fade(0);
}

/// A sheet that starts late, as on a loaded machine, still counts each picture's
/// `at_ms` from the switch, so the fade it shows matches the time it says: counted from
/// the sheet's own start, the fade was further on than `at_ms` said (#1421's CI).
#[test]
fn a_late_sheet_counts_from_the_switch() {
    struct Stall;
    impl Drop for Stall {
        fn drop(&mut self) {
            crate::engine::SHEET_STALL_MS.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let _gpu = super::one_gpu_test_at_a_time();
    crate::engine::SHEET_STALL_MS.store(150, std::sync::atomic::Ordering::Relaxed);
    let _stall = Stall;
    if let Some(first) = watch_the_fade_held(150) {
        assert!(
            first >= 150,
            "the first picture counts from the switch: {first} ms"
        );
    }
}

/// [`a_switch_and_watch_shows_the_fade_as_it_plays`] with the sheet started `stall_ms`
/// after the switch.
fn watch_the_fade(stall_ms: u32) {
    let _gpu = super::one_gpu_test_at_a_time();
    watch_the_fade_held(stall_ms);
}

/// The fade checks, with the GPU test lock held; the first picture's `at_ms`, or `None`
/// with no adapter.
fn watch_the_fade_held(stall_ms: u32) -> Option<u32> {
    let engine = Engine::new();
    let control = connected(&engine);
    two_scenes(&engine, &control);
    let sheet = sheet_or_skip(switch_and_watch(
        &control,
        "b",
        Some("fade"),
        FrameStrip::new(4, 250),
    ))?;
    assert_eq!((sheet.columns, sheet.rows), (2, 2));
    assert_eq!((sheet.tile_width, sheet.tile_height), (480, 270));
    assert_eq!(sheet.frames.len(), 4);
    let times: Vec<u32> = sheet.frames.iter().map(|frame| frame.at_ms).collect();
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
    for (at, wanted) in times.iter().zip([0, 250, 500, 750]) {
        let wanted = wanted + stall_ms;
        assert!(*at >= wanted, "a picture before its time: {times:?}");
    }
    let tiles = centres(&sheet);
    for (frame, [red, green, blue, _]) in sheet.frames.iter().zip(&tiles) {
        let at = frame.at_ms;
        assert_eq!(*blue, 0, "at {at} ms");
        if at < 100 {
            assert!(
                red > green,
                "at {at} ms the fade has barely begun: {red} {green}"
            );
        } else if at < 450 {
            // Both show, whichever space the project blends in: a linear-light mix
            // keeps more red late in the fade than a browser-style (sRGB) one, which
            // is down to about 10% by 400 ms (#1298).
            assert!(
                (1..255).contains(red) && (1..255).contains(green),
                "at {at} ms red and green are mixed: {red} {green}"
            );
        } else if at < 590 {
            // An sRGB mix has no red left by about 570 ms, but green is still rising.
            assert!(
                *green < 255,
                "at {at} ms the fade isn't over: {red} {green}"
            );
        } else if at >= 600 {
            assert_eq!((*red, *green), (0, 255), "at {at} ms the fade is over");
        }
    }
    assert!(
        tiles
            .windows(2)
            .all(|pair| pair[0][1] <= pair[1][1] && pair[0][0] >= pair[1][0]),
        "green only grows and red only fades: {tiles:?}"
    );
    match query(&control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { program_scene, .. }) => {
            assert_eq!(program_scene, Some(SceneId::new("b")), "it switched");
        }
        other => panic!("not studio.get: {other:?}"),
    }
    times.first().copied()
}

/// `video.frames` of a scene off program draws that scene alone, and of the program
/// what is on air.
#[test]
fn video_frames_draws_a_scene_alone_or_the_program() {
    let _gpu = super::one_gpu_test_at_a_time();
    let engine = Engine::new();
    let control = connected(&engine);
    two_scenes(&engine, &control);
    let Some(scene) = sheet_or_skip(frames(&control, FrameStrip::new(2, 10), Some("b"))) else {
        return;
    };
    assert!(
        centres(&scene).iter().all(|c| c[..3] == [0, 255, 0]),
        "green"
    );
    let program = frames(&control, FrameStrip::new(3, 10), None).expect("a sheet");
    assert_eq!((program.columns, program.rows), (2, 2));
    assert!(
        centres(&program).iter().all(|c| c[..3] == [255, 0, 0]),
        "red"
    );
}

/// A sheet past the limits, or of a scene that doesn't exist, is refused, and a refused
/// `scene.switch_and_watch` doesn't switch. These need no adapter.
#[test]
fn a_sheet_out_of_limits_is_refused_before_anything_happens() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, edits(vec![create("a"), create("b")])).expect("built");
    let code = |error: ControlError| error.protocol().map(ProtocolError::code);
    assert_eq!(
        frames(&control, FrameStrip::new(17, 10), None)
            .map(|_| ())
            .map_err(code),
        Err(Some("invalid_argument"))
    );
    assert_eq!(
        frames(&control, FrameStrip::new(2, 10), Some("nope"))
            .map(|_| ())
            .map_err(code),
        Err(Some("not_found"))
    );
    let program = || match query(&control, Query::StudioGet) {
        Ok(QueryResult::StudioGet { program_scene, .. }) => program_scene,
        other => panic!("not studio.get: {other:?}"),
    };
    let before = program();
    assert_eq!(
        switch_and_watch(&control, "b", None, FrameStrip::new(2, 5))
            .map(|_| ())
            .map_err(code),
        Err(Some("invalid_argument"))
    );
    assert_eq!(program(), before, "a refused sheet doesn't switch");
    assert_eq!(
        switch_and_watch(&control, "nope", None, FrameStrip::new(2, 10))
            .map(|_| ())
            .map_err(code),
        Err(Some("not_found"))
    );
}

/// Four sheets at once are taken; a fifth is refused at once with `action_failed`.
#[test]
fn a_fifth_sheet_at_once_is_refused() {
    let engine = Engine::new();
    let control = connected(&engine);
    let call = || Call::Query {
        query: Query::VideoFrames {
            strip: FrameStrip::new(2, 300),
            scene_id: None,
        },
    };
    let running: Vec<_> = (0..crate::frames::MAX_SHEETS)
        .map(|_| control.call(call()))
        .collect();
    let refused = block_on(control.call(call())).map(|_| ());
    let refused = refused.map_err(|error| error.protocol().cloned());
    assert!(
        matches!(&refused, Err(Some(error)) if error == &crate::frames::busy()),
        "a fifth sheet is refused as busy: {refused:?}"
    );
    // The four answer, with a sheet or, without an adapter, an error.
    for answer in running {
        let _ = block_on(answer);
    }
    let again = block_on(control.call(call()));
    assert!(
        again
            .as_ref()
            .err()
            .and_then(ControlError::protocol)
            .is_none_or(|error| is_no_adapter(error) || error.code() != "action_failed"),
        "a sheet is taken again once the others answered: {again:?}"
    );
}

/// #1189: a frame sheet of a scene that is not on program hears the mix. The item's
/// opacity follows that level, dark at silence and full when the scope is loud, so the
/// tile is red. A sheet that evaluates the modifier as silence stays dark.
#[test]
fn a_scene_sheet_hears_audio_motion() {
    let _gpu = super::one_gpu_test_at_a_time();
    let engine = Engine::new();
    let control = connected(&engine);
    let f = |v: f32| Finite32::new(v).expect("finite");
    batch(
        &control,
        edits(vec![
            create("main"),
            create("side"),
            fill("red", "#ff0000ff"),
            add("side", "lamp", "red"),
            Edit::ItemSetMotion {
                item_id: ItemId::new("lamp"),
                scene_id: None,
                motion: vec![MotionModifier {
                    modifier_id: "pulse".into(),
                    property: MotionProperty::Opacity,
                    effect: None,
                    kind: MotionKind::Audio {
                        audio: None,
                        audio_source: None,
                        measure: MotionMeasure::Level,
                        low_hz: None,
                        high_hz: None,
                        floor_db: f(-60.0),
                        ceiling_db: f(-6.0),
                        attack_ms: 0,
                        release_ms: 200,
                        curve: MotionCurve::Linear,
                        from: f(-1.0),
                        to: Finite32::ZERO,
                    },
                    clock: MotionClock::Engine,
                    enabled: true,
                }],
            },
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the colour source was built");
    let samples: Vec<f32> = (0..2048)
        .flat_map(|n| {
            let s = (std::f32::consts::TAU * 440.0 * n as f32 / 48_000.0).sin();
            [s, s]
        })
        .collect();
    engine.audio_scope().push(48_000, &samples);
    let Some(sheet) = sheet_or_skip(frames(&control, FrameStrip::new(2, 20), Some("side"))) else {
        return;
    };
    let pixel = centres(&sheet)[0];
    assert!(
        pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40,
        "the scene sheet should show the loud mix, not silence: {pixel:?}"
    );
}

/// A 200 × 200 box of colour `hex`.
fn small(id: &str, hex: &str) -> Edit {
    let Edit::SourceCreate {
        source_id,
        name,
        kind,
        mut settings,
        duplicated_from,
    } = fill(id, hex)
    else {
        unreachable!("fill creates a source")
    };
    settings.0.insert("width".into(), json!(200));
    settings.0.insert("height".into(), json!(200));
    Edit::SourceCreate {
        source_id,
        name,
        kind,
        settings,
        duplicated_from,
    }
}

/// Item `item` as a 200 × 200 box centred on (`x`, `y`), in keyframe `scene` or its own.
fn boxed(item: &str, scene: Option<&str>, x: f32, y: f32) -> Edit {
    Edit::ItemSetGeometry {
        item_id: ItemId::new(item),
        scene_id: scene.map(SceneId::new),
        geometry: jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(x, y).expect("finite"),
            jstream_protocol::Size::try_new(200.0, 200.0).expect("positive"),
        )
        .expect("valid"),
    }
}

/// The pixel at canvas point (`x`, `y`) of tile `index`, the canvas 1920 × 1080.
fn pixel(sheet: &FrameSheet, rgba: &[u8], index: u32, x: f32, y: f32) -> [u8; 4] {
    let (column, row) = (index % sheet.columns, index / sheet.columns);
    let tx = (x / 1920.0 * sheet.tile_width as f32) as u32;
    let ty = (y / 1080.0 * sheet.tile_height as f32) as u32;
    let px = 4 + column * (sheet.tile_width + 4) + tx;
    let py = 4 + row * (sheet.tile_height + 4) + ty;
    let i = ((py * sheet.width + px) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

/// #1067's done-when (ADR-0129): two keyframes of one scene stagger the same three
/// items in opposite orders, seen in frame strips. Each item jumps down (a `hold`
/// easing over 100 ms) after its keyframe's delay for it: into `k1` red, green, then
/// blue, 400 ms apart; into `k2` blue, green, then red. Each tile is judged by the time
/// it says it was taken, and a tile within 60 ms of a jump is not judged.
#[test]
fn two_keyframes_stagger_three_items_in_opposite_orders_in_a_frame_strip() {
    use jstream_protocol::{ItemEasing, KeyframeTiming};
    let _gpu = super::one_gpu_test_at_a_time();
    let engine = Engine::new();
    let control = connected(&engine);
    let colours = [
        ("r", "#ff0000ff", 480.0, [255, 0, 0]),
        ("g", "#00ff00ff", 960.0, [0, 255, 0]),
        ("b", "#0000ffff", 1440.0, [0, 0, 255]),
    ];
    let (top, bottom) = (270.0, 810.0);
    let mut list = vec![
        create("p"),
        Edit::SceneCreateVariant {
            scene_id: SceneId::new("k1"),
            variant_of: SceneId::new("p"),
            name: "K1".into(),
        },
        Edit::SceneCreateVariant {
            scene_id: SceneId::new("k2"),
            variant_of: SceneId::new("p"),
            name: "K2".into(),
        },
        Edit::TransitionCreate {
            preset_id: TransitionPresetId::new("move"),
            name: "Move".into(),
            kind: "move".into(),
            duration_ms: 100,
            easing: Easing::Linear,
            settings: Settings::default(),
        },
    ];
    // The stock `cut` preset switches back between the strips.
    let delays = [[0, 400, 800], [800, 400, 0]];
    for (n, (item, hex, x, _)) in colours.iter().enumerate() {
        list.push(small(item, hex));
        list.push(add("p", item, item));
        list.push(boxed(item, None, *x, top));
        for (keyframe, delays) in ["k1", "k2"].into_iter().zip(delays) {
            list.push(boxed(item, Some(keyframe), *x, bottom));
            list.push(Edit::ItemSetTiming {
                scene_id: SceneId::new(keyframe),
                item_id: ItemId::new(*item),
                timing: KeyframeTiming {
                    delay_ms: Some(delays[n]),
                    easing: Some(ItemEasing::Hold),
                    duration_ms: Some(100),
                },
            });
        }
    }
    batch(&control, edits(list)).expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
    let to = |scene: &str, preset: &str| {
        let action = Action::SceneSwitch {
            scene_id: SceneId::new(scene),
            preset_id: Some(TransitionPresetId::new(preset)),
        };
        block_on(control.call(Call::Action { action })).expect("switch");
    };
    to("p", "cut");
    for (keyframe, delays) in ["k1", "k2"].into_iter().zip(delays) {
        let Some(sheet) = sheet_or_skip(switch_and_watch(
            &control,
            keyframe,
            Some("move"),
            FrameStrip::new(12, 100),
        )) else {
            return;
        };
        let bytes = decode_base64(&sheet.png).expect("png is base64");
        let (_, _, rgba) = decode_png(&bytes).expect("png decodes");
        let mut judged = [0; 3];
        for (index, frame) in sheet.frames.iter().enumerate() {
            let at = frame.at_ms as f32;
            for (n, (item, _, x, colour)) in colours.iter().enumerate() {
                let jump = (delays[n] + 100) as f32;
                if (at - jump).abs() < 60.0 {
                    continue;
                }
                judged[n] += 1;
                let moved = at > jump;
                let (here, gone) = if moved { (bottom, top) } else { (top, bottom) };
                let shown = pixel(&sheet, &rgba, index as u32, *x, here);
                let left = pixel(&sheet, &rgba, index as u32, *x, gone);
                assert_eq!(
                    shown[..3],
                    colour[..],
                    "into {keyframe} at {at} ms, {item} is {}",
                    if moved { "down" } else { "up" }
                );
                assert_eq!(left[..3], [0, 0, 0], "into {keyframe} at {at} ms");
            }
        }
        assert!(
            judged.iter().all(|&n| n >= 4),
            "each item judged before and after: {judged:?}"
        );
        to("p", "cut");
    }
}

/// Each tile shows a painted source as it was when the picture was taken, not as its
/// frame slot holds it when the sheet is drawn (ADR-0117 §3): the waveform is silent in
/// the first tile and at full scale in the second.
#[test]
fn a_sheet_shows_a_painted_source_as_each_picture_was_taken() {
    let _gpu = super::one_gpu_test_at_a_time();
    let engine = Engine::new();
    let control = connected(&engine);
    let settings = [
        ("width", json!(1920)),
        ("height", json!(1080)),
        ("color", json!("#ff0000")),
        ("background", json!("#0000ff")),
    ];
    batch(
        &control,
        edits(vec![
            create("a"),
            Edit::SourceCreate {
                source_id: SourceId::new("wave"),
                name: "wave".into(),
                kind: SourceKind::Builtin {
                    kind: "waveform".into(),
                },
                settings: Settings(
                    settings
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), v.clone()))
                        .collect(),
                ),
                duplicated_from: None,
            },
            add("a", "i1", "wave"),
        ]),
    )
    .expect("built");
    assert!(engine.settle_sources(), "the sources were built in time");
    let scope = engine.audio_scope();
    let feeding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let feeder = {
        let feeding = std::sync::Arc::clone(&feeding);
        std::thread::spawn(move || {
            // After the first picture, and on until the sheet is drawn.
            std::thread::sleep(std::time::Duration::from_millis(200));
            while feeding.load(std::sync::atomic::Ordering::Relaxed) {
                scope.push(48_000, &[1.0; 960]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })
    };
    let sheet = sheet_or_skip(frames(&control, FrameStrip::new(2, 800), None));
    feeding.store(false, std::sync::atomic::Ordering::Relaxed);
    feeder.join().expect("fed");
    let Some(sheet) = sheet else {
        return;
    };
    // A silent waveform draws its line across the middle; one at full scale, at the top.
    let (top, middle) = (at_height(&sheet, 0), centres(&sheet));
    assert!(
        top[0][0] == 0 && middle[0][0] > 0,
        "silent when taken: {top:?} {middle:?}"
    );
    assert!(
        top[1][0] > 0 && middle[1][0] == 0,
        "full scale when taken: {top:?} {middle:?}"
    );
}

/// The pixel `y` rows into each tile, half-way across, in order.
fn at_height(sheet: &FrameSheet, y: u32) -> Vec<[u8; 4]> {
    let bytes = decode_base64(&sheet.png).expect("png is base64");
    let (width, _, rgba) = decode_png(&bytes).expect("png decodes");
    (0..sheet.frames.len() as u32)
        .map(|index| {
            let (column, row) = (index % sheet.columns, index / sheet.columns);
            let x = 4 + column * (sheet.tile_width + 4) + sheet.tile_width / 2;
            let y = 4 + row * (sheet.tile_height + 4) + y;
            let i = ((y * width + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        })
        .collect()
}
