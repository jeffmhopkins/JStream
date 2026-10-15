//! The `media` kind on the platform's fake media files (R-TEST-004): frames on the
//! clock, looping, speed, restarting when shown, live updates and its errors.
//!
//! Decoding runs on the source's own thread, so a test waits for it to catch up before
//! it reads which frame is shown ([`shown`]).

use jstream_media::{FrameSize, MediaTime, Timebase};
use jstream_platform::fake::{FakeClip, FakeMediaDecoders};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Limits, Registry, Source, SourceError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn ms(t: i64) -> MediaTime {
    MediaTime::new(t, Timebase::MILLIS)
}

/// A fresh, empty folder for one test.
fn folder(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("media-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp folder");
    dir
}

/// Ten 8x4 frames at 25 fps, 400 ms in all, with a key frame every 4.
fn clip() -> FakeClip {
    FakeClip {
        size: FrameSize::new(8, 4).expect("a size"),
        frame_period: Timebase::new(1, 25).expect("a time base"),
        frames: 10,
        key_every: 4,
        audio: false,
    }
}

/// A folder holding `clip.mp4`, which `fake` plays as `clip`.
fn setup(name: &str, clip: FakeClip) -> (PathBuf, FakeMediaDecoders) {
    let dir = folder(name);
    std::fs::write(dir.join("clip.mp4"), b"fake").expect("a file");
    let fake = FakeMediaDecoders::new();
    let host = dir.join("clip.mp4").canonicalize().expect("the file");
    fake.add(host, clip);
    (dir, fake)
}

fn ctx(dir: &Path, fake: &FakeMediaDecoders) -> Context {
    Context {
        assets: AssetRoots {
            collection: Some(dir.to_owned()),
            app: None,
        },
        limits: Limits::default(),
        audio: None,
        media: Some(Arc::new(fake.clone())),
    }
}

fn settings(file: Option<&str>, looping: bool, speed: f64, restart: bool) -> Settings {
    let file = file.map_or(Value::Null, |path| {
        Reference::Asset {
            root: AssetRoot::Collection,
            path: path.into(),
        }
        .to_value()
    });
    [
        ("file", file),
        ("loop", json!(looping)),
        ("speed", json!(speed)),
        ("restart_on_show", json!(restart)),
    ]
    .into_iter()
    .collect()
}

fn create(ctx: &Context, settings: &Settings) -> Result<Box<dyn Source>, SourceError> {
    Registry::builtin().create("media", settings, ctx)
}

/// The index of the clip frame shown at `t` ms, once the decode thread has caught up:
/// the next frame is decoded and not due yet, or the file has ended. A frame queued
/// while `frame` ran is due at once (`t` ms and 1 µs), and the decoder still behind is
/// asked about again 10 ms later.
fn shown(source: &mut Box<dyn Source>, t: i64) -> u32 {
    let at = ms(t);
    let due = MediaTime::new(t * 1000 + 1, Timebase::MICROS);
    let retry = MediaTime::new(t * 1000 + 10_000, Timebase::MICROS);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = source.frame(at).expect("a frame");
        let next = source.next_change(at);
        if next != Some(due) && next != Some(retry) {
            let red = frame.plane(0).expect("a plane").data()[0];
            return u32::from(red);
        }
        assert!(
            Instant::now() < deadline,
            "the decoder never caught up with {t} ms"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_video_plays_on_the_clock_and_holds_its_last_frame() {
    let (dir, fake) = setup("plays", clip());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 1.0, true),
    )
    .expect("a source");
    let frame = source.frame(ms(0)).expect("the first frame");
    assert_eq!(frame.info().size, FrameSize::new(8, 4).expect("a size"));
    assert_eq!(
        frame.plane(0).expect("a plane").data()[..4],
        FakeClip::pixel(0)
    );
    assert_eq!([shown(&mut source, 0), shown(&mut source, 39)], [0, 0]);
    assert_eq!(source.next_change(ms(39)), Some(ms(40)));
    assert_eq!([shown(&mut source, 40), shown(&mut source, 130)], [1, 3]);
    assert_eq!(shown(&mut source, 399), 9);
    assert_eq!(shown(&mut source, 5000), 9, "the last frame stays");
    assert_eq!(source.next_change(ms(5000)), None);
}

#[test]
fn a_looping_video_starts_again_and_catches_up_after_a_gap() {
    let (dir, fake) = setup("loops", clip());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), true, 1.0, true),
    )
    .expect("a source");
    assert_eq!(shown(&mut source, 399), 9);
    assert_eq!([shown(&mut source, 400), shown(&mut source, 440)], [0, 1]);
    let seeks = fake.stats().seeks;
    // Seconds later (the program didn't show it meanwhile): it seeks rather than
    // decoding every frame in between. 10 * 400 + 250 ms is frame 6 of the 11th play.
    assert_eq!(shown(&mut source, 4250), 6);
    assert!(fake.stats().seeks > seeks, "it sought ahead");
}

#[test]
fn speed_scales_the_timeline_and_a_change_keeps_the_position() {
    let (dir, fake) = setup("speed", clip());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 2.0, true),
    )
    .expect("a source");
    assert_eq!(shown(&mut source, 40), 2);
    source
        .update(&settings(Some("clip.mp4"), false, 1.0, true))
        .expect("speed is live");
    // 40 ms in, the timeline was at 80 ms; 40 ms later at normal speed it is at 120.
    assert_eq!(shown(&mut source, 80), 3);
    assert_eq!(source.next_change(ms(80)), Some(ms(120)));
}

#[test]
fn showing_it_again_restarts_it_when_asked() {
    let (dir, fake) = setup("restart", clip());
    let mut restarts = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), true, 1.0, true),
    )
    .expect("a source");
    let mut keeps = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), true, 1.0, false),
    )
    .expect("a source");
    assert_eq!([shown(&mut restarts, 200), shown(&mut keeps, 200)], [5, 5]);
    restarts.show(ms(300));
    keeps.show(ms(300));
    assert_eq!(shown(&mut restarts, 300), 0, "it plays from the start");
    assert_eq!(shown(&mut restarts, 340), 1);
    assert_eq!(shown(&mut keeps, 300), 7, "it plays on");
}

#[test]
fn turning_loop_on_after_the_end_plays_on() {
    let (dir, fake) = setup("loop-on", clip());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 1.0, true),
    )
    .expect("a source");
    assert_eq!(shown(&mut source, 1000), 9);
    assert_eq!(source.next_change(ms(1000)), None);
    source
        .update(&settings(Some("clip.mp4"), true, 1.0, true))
        .expect("loop is live");
    // 1000 ms is 200 ms into the third play: frame 5.
    assert_eq!(shown(&mut source, 1000), 5);
    assert_eq!(shown(&mut source, 1040), 6);
}

#[test]
fn no_file_shows_one_transparent_pixel_and_the_file_is_not_live() {
    let (dir, fake) = setup("empty", clip());
    let mut source = create(&ctx(&dir, &fake), &settings(None, true, 1.0, true)).expect("a source");
    let frame = source.frame(ms(0)).expect("a frame");
    assert_eq!(frame.info().size, FrameSize::new(1, 1).expect("a size"));
    assert_eq!(frame.plane(0).expect("a plane").data(), &[0, 0, 0, 0]);
    assert_eq!(source.next_change(ms(0)), None);
    assert!(matches!(
        source.update(&settings(Some("clip.mp4"), true, 1.0, true)),
        Err(SourceError::NotLive { param }) if param == "file"
    ));
    assert_eq!(fake.stats().opened, 0);
}

#[test]
fn files_it_cant_play_are_errors() {
    let (dir, fake) = setup("errors", clip());
    std::fs::write(dir.join("other.mp4"), b"not one the fake knows").expect("a file");
    std::fs::write(dir.join("song.mp3"), b"audio only").expect("a file");
    fake.add(
        dir.join("song.mp3").canonicalize().expect("the file"),
        FakeClip {
            frames: 0,
            audio: true,
            ..clip()
        },
    );
    let ctx = ctx(&dir, &fake);
    let err = |file: &str| {
        create(&ctx, &settings(Some(file), true, 1.0, true))
            .err()
            .expect("an error")
    };
    assert!(matches!(
        err("other.mp4"),
        SourceError::Decode { path, .. } if path == "other.mp4"
    ));
    assert!(matches!(
        err("missing.mp4"),
        SourceError::AssetRead {
            kind: std::io::ErrorKind::NotFound,
            ..
        }
    ));
    assert!(matches!(
        err("song.mp3"),
        SourceError::Decode { reason, .. } if reason.contains("no video")
    ));
    assert!(matches!(
        err("../outside.mp4"),
        SourceError::AssetRead { .. } | SourceError::AssetEscapesRoot { .. }
    ));

    let no_backend = Context {
        media: None,
        ..ctx.clone()
    };
    assert!(matches!(
        create(&no_backend, &settings(Some("clip.mp4"), true, 1.0, true)),
        Err(SourceError::NoBackend { .. })
    ));
    assert!(matches!(
        create(&ctx, &settings(Some("clip.mp4"), true, 9.0, true)),
        Err(SourceError::Setting { param, .. }) if param == "speed"
    ));
}

#[test]
fn frames_past_the_limits_are_refused() {
    let (dir, fake) = setup("limits", clip());
    let file = settings(Some("clip.mp4"), true, 1.0, true);
    let small_side = Context {
        limits: Limits {
            max_side: 4,
            ..Limits::default()
        },
        ..ctx(&dir, &fake)
    };
    assert!(matches!(
        create(&small_side, &file),
        Err(SourceError::TooLarge { .. })
    ));
    // 8x4 RGBA is 128 bytes a frame: four fit the queue's one and the three others held.
    let small_memory = |bytes| Context {
        limits: Limits {
            max_decoded_bytes: bytes,
            ..Limits::default()
        },
        ..ctx(&dir, &fake)
    };
    assert!(matches!(
        create(&small_memory(3 * 128), &file),
        Err(SourceError::TooLarge { .. })
    ));
    let mut source = create(&small_memory(4 * 128), &file).expect("one frame ahead fits");
    assert_eq!([shown(&mut source, 0), shown(&mut source, 80)], [0, 2]);
}
