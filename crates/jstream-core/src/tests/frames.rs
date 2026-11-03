//! Frame sheets (#1061): `video.frames` and `scene.switch_and_watch` take their pictures
//! in real time, tile them in order, and say when each was taken.

use super::{add, batch, block_on, connected, create, edits, query};
use crate::Engine;
use crate::snapshot::{decode_base64, decode_png, is_no_adapter};
use jstream_control::{Control, ControlError};
use jstream_protocol::{
    Action, ActionResult, Call, CallResult, Easing, Edit, FrameSheet, FrameStrip, ProtocolError,
    Query, QueryResult, SceneId, Settings, SourceId, SourceKind, TransitionPresetId,
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
    let _gpu = super::one_gpu_test_at_a_time();
    let engine = Engine::new();
    let control = connected(&engine);
    two_scenes(&engine, &control);
    let Some(sheet) = sheet_or_skip(switch_and_watch(
        &control,
        "b",
        Some("fade"),
        FrameStrip::new(4, 250),
    )) else {
        return;
    };
    assert_eq!((sheet.columns, sheet.rows), (2, 2));
    assert_eq!((sheet.tile_width, sheet.tile_height), (480, 270));
    assert_eq!(sheet.frames.len(), 4);
    let times: Vec<u32> = sheet.frames.iter().map(|frame| frame.at_ms).collect();
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
    for (at, wanted) in times.iter().zip([0, 250, 500, 750]) {
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
            assert!(
                (32..224).contains(red) && (32..224).contains(green),
                "at {at} ms red and green are mixed: {red} {green}"
            );
        } else if at < 590 {
            assert!(
                *red > 0 && *green < 255,
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
    assert_eq!(
        refused.map_err(|error| error.protocol().map(ProtocolError::code)),
        Err(Some("action_failed"))
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
            .is_none_or(|error| error.code() != "action_failed"),
        "a sheet is taken again once the others answered: {again:?}"
    );
}
