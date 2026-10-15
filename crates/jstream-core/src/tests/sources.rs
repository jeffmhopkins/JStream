//! Live sources (C1b): the engine builds a `jstream-sources` source for each source in
//! the collection, rebuilds it when its settings change, drops it when the source is
//! deleted, and the program mix shows its frames, advancing animations on the clock.

use super::{add, batch, connected, create, edits};
use crate::Engine;
use jstream_media::Timebase;
use jstream_platform::fake::FakeClock;
use jstream_protocol::{AssetRoot, Batch, Edit, Reference, Settings, SourceId, SourceKind};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Pillow's 4x4 animated WebP from `jstream-sources`' tests: red for 100 ms, green for
/// 200 ms, then blue at alpha 128 for 300 ms, twice.
const ANIMATED: &[u8] = include_bytes!("../../../jstream-sources/tests/data/anim.webp");

fn settings(pairs: &[(&str, Value)]) -> Settings {
    Settings(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn source(id: &str, kind: &str, settings: Settings) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin { kind: kind.into() },
        settings,
        duplicated_from: None,
    }
}

fn color(hex: &str) -> Settings {
    settings(&[
        ("color", json!(hex)),
        ("width", json!(2)),
        ("height", json!(2)),
    ])
}

/// The first pixel the published program mix shows for `id`, once every build is done.
fn shown(engine: &Engine, id: &str) -> Option<[u8; 4]> {
    assert!(engine.settle_sources(), "the sources were built in time");
    engine
        .render_hub()
        .load()
        .shot()
        .frame(&SourceId::new(id))
        .and_then(|frame| frame.first_pixel())
}

#[test]
fn a_colour_source_is_built_rebuilt_and_dropped() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("fill", "color", color("#ff8000ff")),
            add("a", "i1", "fill"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "fill"), Some([0xff, 0x80, 0x00, 0xff]));

    batch(
        &control,
        Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("fill"),
            patch: settings(&[("color", json!("#10203040"))]),
        }),
    )
    .expect("changed");
    assert_eq!(
        shown(&engine, "fill"),
        Some([0x10, 0x20, 0x30, 0x40]),
        "a settings change rebuilds the source"
    );

    batch(
        &control,
        Batch::single(Edit::ItemRemove {
            item_id: jstream_protocol::ItemId::new("i1"),
        }),
    )
    .expect("removed");
    assert_eq!(
        shown(&engine, "fill"),
        None,
        "a source no item places is not drawn"
    );
    batch(
        &control,
        Batch::single(Edit::SourceDelete {
            source_id: SourceId::new("fill"),
        }),
    )
    .expect("deleted");
    assert_eq!(shown(&engine, "fill"), None);
}

#[test]
fn a_stored_frame_wins_over_the_live_source() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("fill", "color", color("#ff0000ff")),
            add("a", "i1", "fill"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "fill"), Some([0xff, 0, 0, 0xff]));
    engine
        .set_source_frame(&SourceId::new("fill"), 1, 1, &[1, 2, 3, 4])
        .expect("a frame");
    assert_eq!(shown(&engine, "fill"), Some([1, 2, 3, 4]));
}

#[test]
fn a_text_source_is_built_and_rebuilt() {
    let engine = Engine::new();
    let control = connected(&engine);
    let text = |background: &str| {
        settings(&[
            ("text", json!("Hi")),
            ("background", json!(background)),
            ("padding", json!(4)),
        ])
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("caption", "text", text("#00ff00ff")),
            add("a", "i1", "caption"),
        ]),
    )
    .expect("created");
    // The top left corner is padding, so it shows the background.
    assert_eq!(shown(&engine, "caption"), Some([0, 0xff, 0, 0xff]));

    batch(
        &control,
        Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("caption"),
            patch: settings(&[("background", json!("#0000ffff"))]),
        }),
    )
    .expect("changed");
    assert_eq!(shown(&engine, "caption"), Some([0, 0, 0xff, 0xff]));
}

#[test]
fn a_kind_that_is_not_built_still_shows_the_capture() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            source("screen", "display", Settings::new()),
            add("a", "i1", "screen"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "screen"), None);
    assert!(engine.render_hub().load().shot().uses_capture());
}

/// A new, empty directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jstream-core-sources-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Which channel of the image's first pixel is lit, and its alpha. The lossless frames
/// come back with a channel a step off (255 as 254), so the test reads the dominant one.
fn colour(engine: &Engine) -> (usize, u8) {
    let pixel = shown(engine, "gif").expect("a frame");
    let lit = (0..3).max_by_key(|&c| pixel[c]).expect("three channels");
    (lit, pixel[3])
}

#[test]
fn an_animated_image_advances_on_the_clock() {
    let dir = temp_dir("animated");
    fs::write(dir.join("anim.webp"), ANIMATED).expect("the image");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "anim.webp".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("gif", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "gif"),
        ]),
    )
    .expect("created");
    assert_eq!(colour(&engine), (0, 0xff), "red first");

    clock.advance(99);
    engine.replan();
    assert_eq!(colour(&engine), (0, 0xff), "still red");
    clock.advance(1);
    engine.replan();
    assert_eq!(colour(&engine), (1, 0xff), "green at 100 ms");
    // Past both plays (2 x 600 ms), the last frame stays.
    clock.advance(2_000);
    engine.replan();
    assert_eq!(colour(&engine), (2, 128), "blue at alpha 128");
    let _ = fs::remove_dir_all(dir);
}

/// #224: a re-plan timed on the old clock must not hold an animation back on the new one.
#[test]
fn an_animation_keeps_advancing_after_a_clock_swap() {
    let dir = temp_dir("clock-swap");
    fs::write(dir.join("anim.webp"), ANIMATED).expect("the image");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    // The old clock is far ahead, so its next frame is due long after the new clock's
    // zero.
    let old = FakeClock::new(Timebase::MILLIS);
    old.advance(1_000_000);
    engine.set_source_clock(Arc::new(old));
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "anim.webp".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("gif", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "gif"),
        ]),
    )
    .expect("created");
    assert_eq!(colour(&engine), (0, 0xff), "red first");

    let new = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(new.clone()));
    new.advance(100);
    // Only the sources' own thread re-plans here; it checks the clock at least once a
    // second.
    let deadline = Instant::now() + Duration::from_secs(10);
    while colour(&engine) != (1, 0xff) {
        assert!(
            Instant::now() < deadline,
            "green at 100 ms on the new clock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_missing_file_shows_nothing() {
    let dir = temp_dir("missing");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "nowhere.png".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("pic", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "pic"),
        ]),
    )
    .expect("an asset that doesn't exist yet is accepted");
    assert_eq!(shown(&engine, "pic"), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_sources_thread_advances_an_animation_by_itself() {
    let dir = temp_dir("by-itself");
    fs::write(dir.join("anim.webp"), ANIMATED).expect("the image");
    let engine = Engine::new()
        .with_collection_file(dir.join("scenes.json"))
        .expect("collection file");
    let control = connected(&engine);
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: "anim.webp".into(),
    };
    batch(
        &control,
        edits(vec![
            create("a"),
            source("gif", "image", settings(&[("file", file.to_value())])),
            add("a", "i1", "gif"),
        ]),
    )
    .expect("created");
    // On the engine's own clock the thread re-plans when each frame is due; no call
    // touches the engine meanwhile.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = Vec::new();
    while std::time::Instant::now() < deadline {
        let now = colour(&engine);
        if seen.last() != Some(&now) {
            seen.push(now);
        }
        if now == (2, 128) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        seen.last(),
        Some(&(2, 128)),
        "reached the last frame: {seen:?}"
    );
    assert!(
        seen.contains(&(1, 0xff)),
        "showed green on the way: {seen:?}"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_waveform_source_is_painted_by_the_sources_thread() {
    let engine = Engine::new();
    let control = connected(&engine);
    // One column, three rows: silence lights the middle row, full scale the top one.
    batch(
        &control,
        edits(vec![
            create("a"),
            source(
                "wave",
                "waveform",
                settings(&[
                    ("width", json!(1)),
                    ("height", json!(3)),
                    ("color", json!("#ff0000")),
                    ("background", json!("#0000ff")),
                ]),
            ),
            add("a", "i1", "wave"),
        ]),
    )
    .expect("created");
    assert_eq!(shown(&engine, "wave"), Some([0, 0, 0xff, 0xff]), "silence");
    // The program mix's audio arrives: the thread paints it on its own, off the lock.
    let scope = engine.audio_scope();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && shown(&engine, "wave") != Some([0xff, 0, 0, 0xff]) {
        scope.push(48_000, &[1.0; 960]);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        shown(&engine, "wave"),
        Some([0xff, 0, 0, 0xff]),
        "full scale"
    );
}
