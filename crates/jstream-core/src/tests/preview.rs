//! Preview streams (U1, ADR-0045): `preview.subscribe` sends the program mix to the
//! session that asked, as binary frames on its channel, in H.264 when the platform has
//! an encoder and JPEG otherwise; a reader that falls behind loses frames up to a
//! keyframe, and the streams end with `preview.unsubscribe` or the session. A stream may
//! show one scene instead, planned for it alone, and ends when the scene is deleted.
//! The preview captures the displays, windows and webcams the program and those scenes
//! show itself.

use super::{add, batch, block_on, code, connected, create, edits, live, moved, query, webcam};
use crate::preview::{BINARY_BUFFER, KEYFRAME_EVERY, OpenPreviewDraw, PreviewDraw};
use crate::snapshot::{Captures, LiveParams, Shot};
use crate::{Engine, InProcessControl};
use jstream_control::{BinaryStream, Control, ControlError};
use jstream_platform::fake::{self, FakeClock};
use jstream_protocol::{
    Action, Batch, BinaryFrame, Call, Edit, PreviewCodec, PreviewDraft, ProtocolError, Query,
    QueryResult, Rev, SceneId, SourceId, SourceKind, WireError,
};
use jstream_sources::CaptureSpec;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::{Duration, Instant};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;

/// Long enough for the preview thread to draw on a loaded CI machine.
const WAIT: Duration = Duration::from_secs(10);

/// What a binary stream gave within a wait.
#[derive(Debug)]
enum Next {
    Frame(BinaryFrame),
    Ended,
    Nothing,
}

/// The next frame `frames` yields within `within`.
fn next(frames: &mut BinaryStream, within: Duration) -> Next {
    struct Unpark(thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let deadline = Instant::now() + within;
    loop {
        match frames.as_mut().poll_next(&mut Context::from_waker(&waker)) {
            Poll::Ready(Some(frame)) => return Next::Frame(frame),
            Poll::Ready(None) => return Next::Ended,
            Poll::Pending => {
                let now = Instant::now();
                if now >= deadline {
                    return Next::Nothing;
                }
                thread::park_timeout(deadline - now);
            }
        }
    }
}

fn frame(frames: &mut BinaryStream) -> BinaryFrame {
    match next(frames, WAIT) {
        Next::Frame(frame) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

/// The next frame showing `rev` or later.
fn frame_at(frames: &mut BinaryStream, rev: Rev) -> BinaryFrame {
    let deadline = Instant::now() + WAIT;
    loop {
        let frame = frame(frames);
        if frame.header.rev >= rev {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame at {rev}");
    }
}

fn subscribe(channel: u32, fps: Option<u32>, codecs: Vec<PreviewCodec>) -> Call {
    Call::Action {
        action: Action::PreviewSubscribe {
            channel,
            max_width: None,
            max_height: None,
            fps,
            codecs,
            scene_id: None,
            source: None,
            source_id: None,
        },
    }
}

/// [`subscribe`] to the scene `scene` at 30 fps.
fn subscribe_scene(channel: u32, scene: &str) -> Call {
    Call::Action {
        action: Action::PreviewSubscribe {
            channel,
            max_width: None,
            max_height: None,
            fps: Some(30),
            codecs: Vec::new(),
            scene_id: Some(SceneId::new(scene)),
            source: None,
            source_id: None,
        },
    }
}

fn act(control: &dyn Control, call: Call) -> Result<(), ControlError> {
    block_on(control.call(call)).map(drop)
}

/// The engine with the platform fakes (whose codecs include an H.264 encoder) and the
/// CPU stand-ins for both compositors.
fn with_h264() -> Engine {
    let clock = FakeClock::new(jstream_media::Timebase::MICROS);
    Engine::new()
        .with_backends(fake::backends(&clock).expect("fake backends"))
        .with_cpu_compositor()
}

fn decode(jpeg: &[u8]) -> (usize, usize) {
    let mut decoder = JpegDecoder::new(ZCursor::new(jpeg));
    decoder.decode().expect("decodes");
    let info = decoder.info().expect("has info");
    (usize::from(info.width), usize::from(info.height))
}

/// A scene placing the `webcam` source, so a frame stored for it changes the mix.
fn placed_webcam(control: &dyn Control) -> Rev {
    let scene = vec![create("scn"), webcam(), add("scn", "itm", "webcam")];
    batch(control, edits(scene)).expect("scene")
}

#[test]
fn a_session_needs_its_binary_channel_to_subscribe() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let refused = act(&control, subscribe(1, None, Vec::new())).expect_err("no channel");
    assert_eq!(code(&refused), Some("action_failed"));
}

#[test]
fn without_an_encoder_the_preview_is_jpeg_and_follows_the_mix() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    act(&control, subscribe(7, Some(30), Vec::new())).expect("subscribed");
    let first = frame(&mut frames);
    let header = first.header;
    assert_eq!(header.channel, 7);
    assert_eq!(header.codec, Some(PreviewCodec::Jpeg));
    assert!(header.keyframe && !header.end);
    // The default 1920 × 1080 canvas in the default 640 × 360 box.
    assert_eq!((header.width, header.height), (640, 360));
    assert_eq!(header.rev, Rev(0));
    assert_eq!(decode(&first.payload), (640, 360));

    // A change to what the mix draws is sent, showing its rev.
    let rev = placed_webcam(&control);
    engine
        .set_source_frame(&SourceId::new("webcam"), 2, 2, &[255; 16])
        .expect("frame");
    let changed = frame_at(&mut frames, rev);
    assert!(changed.header.pts_us > header.pts_us);

    // A new canvas size is a new picture size.
    let square = Edit::CanvasSetSize {
        width: 1000,
        height: 1000,
    };
    let rev = batch(&control, Batch::single(square)).expect("canvas");
    let resized = frame_at(&mut frames, rev);
    assert_eq!((resized.header.width, resized.header.height), (360, 360));
    assert_eq!(decode(&resized.payload), (360, 360));
}

#[test]
fn with_an_encoder_the_preview_is_h264_starting_at_a_keyframe() {
    let engine = with_h264();
    let control = connected(&engine);
    let mut frames = control.binary();
    act(&control, subscribe(1, Some(30), Vec::new())).expect("subscribed");
    let first = frame(&mut frames);
    assert_eq!(first.header.codec, Some(PreviewCodec::H264));
    assert!(first.header.keyframe);
    assert_eq!((first.header.width, first.header.height), (640, 360));
    assert!(!first.payload.is_empty());

    // A client that asks for JPEG gets it, from the same engine.
    act(&control, subscribe(2, None, vec![PreviewCodec::Jpeg])).expect("subscribed");
    let deadline = Instant::now() + WAIT;
    loop {
        let frame = frame(&mut frames);
        if frame.header.channel == 2 {
            assert_eq!(frame.header.codec, Some(PreviewCodec::Jpeg));
            break;
        }
        assert!(Instant::now() < deadline, "no frame on channel 2");
    }
}

#[test]
fn subscriptions_are_checked() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let _frames = control.binary();
    let field = |error: ControlError| match error {
        ControlError::Protocol(WireError {
            error: ProtocolError::InvalidArgument { field, .. },
            ..
        }) => field,
        other => panic!("expected invalid_argument, got {other:?}"),
    };
    let too_fast = act(&control, subscribe(1, Some(31), Vec::new())).expect_err("fps");
    assert_eq!(field(too_fast), "fps");
    // No H.264 encoder without backends, and the client takes nothing else.
    let h264_only = act(&control, subscribe(1, None, vec![PreviewCodec::H264])).expect_err("codec");
    assert_eq!(code(&h264_only), Some("action_failed"));

    for channel in 0..16 {
        act(&control, subscribe(channel, Some(1), Vec::new())).expect("subscribed");
    }
    let again = act(&control, subscribe(0, None, Vec::new())).expect_err("in use");
    assert_eq!(field(again), "channel");
    let seventeenth = act(&control, subscribe(99, None, Vec::new())).expect_err("too many");
    assert_eq!(field(seventeenth), "channel");

    // Closing one frees its channel and its place; closing one not open succeeds.
    let close = |channel| Call::Action {
        action: Action::PreviewUnsubscribe { channel },
    };
    act(&control, close(0)).expect("unsubscribed");
    act(&control, close(0)).expect("unsubscribed twice");
    act(&control, subscribe(0, Some(1), Vec::new())).expect("subscribed again");
}

#[test]
fn streams_stop_with_unsubscribe_and_end_with_the_session() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    act(&control, subscribe(3, Some(30), Vec::new())).expect("subscribed");
    frame(&mut frames);
    act(
        &control,
        Call::Action {
            action: Action::PreviewUnsubscribe { channel: 3 },
        },
    )
    .expect("unsubscribed");
    // At most a frame already being queued, then nothing, even across a change.
    let rev = placed_webcam(&control);
    let deadline = Instant::now() + Duration::from_millis(500);
    while let Next::Frame(frame) = next(&mut frames, Duration::from_millis(100)) {
        assert!(frame.header.rev < rev, "a frame after unsubscribe");
        assert!(Instant::now() < deadline, "frames keep coming");
    }

    // Reopening the binary channel ends the one before.
    let mut reopened = control.binary();
    assert!(matches!(next(&mut frames, WAIT), Next::Ended));
    act(&control, subscribe(3, Some(30), Vec::new())).expect("subscribed");
    frame(&mut reopened);
    drop(control);
    let ended = loop {
        match next(&mut reopened, WAIT) {
            Next::Frame(_) => {}
            other => break other,
        }
    };
    assert!(matches!(ended, Next::Ended), "{ended:?}");
}

/// A reader that stops reading holds at most [`BINARY_BUFFER`] frames: the preview
/// thread draws nothing for it while its queue is full, so what it reads afterwards
/// goes on from where it stopped and still decodes.
#[test]
fn a_slow_reader_holds_a_bounded_queue_and_resumes() {
    let engine = with_h264();
    let control: InProcessControl = connected(&engine);
    let mut frames = control.binary();
    act(&control, subscribe(1, Some(30), Vec::new())).expect("subscribed");
    placed_webcam(&control);
    let webcam = SourceId::new("webcam");
    // Change the mix every frame for a while without reading.
    let until = Instant::now() + Duration::from_millis(1500);
    let mut shade = 0_u8;
    while Instant::now() < until {
        shade = shade.wrapping_add(1);
        engine
            .set_source_frame(&webcam, 2, 2, &[shade; 16])
            .expect("frame");
        thread::sleep(Duration::from_millis(20));
    }
    let mut queued = Vec::new();
    while let Next::Frame(frame) = next(&mut frames, Duration::ZERO) {
        queued.push(frame);
    }
    assert!(
        !queued.is_empty() && queued.len() <= BINARY_BUFFER,
        "{}",
        queued.len()
    );
    assert!(queued[0].header.keyframe);
    // The reader catches up and the stream goes on where it left off.
    engine
        .set_source_frame(&webcam, 2, 2, &[shade.wrapping_add(1); 16])
        .expect("frame");
    queued.push(frame(&mut frames));
    assert!(
        queued
            .windows(2)
            .all(|pair| pair[0].header.pts_us < pair[1].header.pts_us),
        "frames out of order"
    );
}

/// A live value (ADR-0032) is drawn into the preview as it is into the program, with no
/// new rev: a `live.set` makes the stream due at once, not at its next keyframe.
#[test]
fn a_live_value_is_sent_without_waiting_for_a_keyframe() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    act(&control, subscribe(1, Some(30), Vec::new())).expect("subscribed");
    let rev = placed_webcam(&control);
    let placed = frame_at(&mut frames, rev);
    act(&control, live("drag", vec![moved("itm", 100.0)])).expect("live.set");
    let dragged = frame(&mut frames);
    assert_eq!(dragged.header.rev, rev);
    let after = Duration::from_micros(
        u64::try_from(dragged.header.pts_us - placed.header.pts_us).expect("later"),
    );
    assert!(
        after < KEYFRAME_EVERY,
        "drawn only for a keyframe: {after:?}"
    );
}

/// A stream of a scene that is not on program shows it, planned for the stream alone:
/// it follows what that scene draws, the engine stops planning it once the stream
/// closes, and a scene the collection doesn't have is refused.
#[test]
fn a_scene_stream_shows_a_scene_off_program() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    let missing = act(&control, subscribe_scene(1, "nowhere")).expect_err("no such scene");
    assert_eq!(code(&missing), Some("not_found"));

    // "main" is on program; "scn" places the webcam and is not.
    batch(&control, edits(vec![create("main")])).expect("program scene");
    let rev = placed_webcam(&control);
    act(&control, subscribe_scene(2, "scn")).expect("subscribed");
    assert_eq!(engine.previewed_scenes(), vec![SceneId::new("scn")]);
    let first = frame_at(&mut frames, rev);
    assert_eq!(first.header.channel, 2);
    assert_eq!((first.header.width, first.header.height), (640, 360));

    // A new frame of a source only that scene shows is a new picture, with no new rev.
    let first_pts = first.header.pts_us;
    engine
        .set_source_frame(&SourceId::new("webcam"), 2, 2, &[255; 16])
        .expect("frame");
    let changed = frame(&mut frames);
    assert!(changed.header.pts_us > first_pts);
    assert!(
        Duration::from_micros(u64::try_from(changed.header.pts_us - first_pts).expect("later"))
            < KEYFRAME_EVERY,
        "drawn only for a keyframe"
    );

    act(
        &control,
        Call::Action {
            action: Action::PreviewUnsubscribe { channel: 2 },
        },
    )
    .expect("unsubscribed");
    assert!(engine.previewed_scenes().is_empty());
}

/// Deleting a scene ends its streams with an end frame that says why.
#[test]
fn deleting_the_scene_ends_its_stream() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    batch(&control, edits(vec![create("main"), create("other")])).expect("scenes");
    act(&control, subscribe_scene(4, "other")).expect("subscribed");
    frame(&mut frames);
    batch(
        &control,
        Batch::single(Edit::SceneDelete {
            scene_id: SceneId::new("other"),
        }),
    )
    .expect("deleted");
    let deadline = Instant::now() + WAIT;
    let end = loop {
        let frame = frame(&mut frames);
        if frame.header.end {
            break frame;
        }
        assert!(Instant::now() < deadline, "no end frame");
    };
    assert_eq!(end.header.channel, 4);
    assert_eq!(&*end.payload, b"the scene was deleted");
    assert!(engine.previewed_scenes().is_empty());
}

/// A session's end stops planning the scenes its streams showed.
#[test]
fn a_session_end_stops_planning_its_scenes() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    batch(&control, edits(vec![create("main"), create("other")])).expect("scenes");
    act(&control, subscribe_scene(4, "other")).expect("subscribed");
    frame(&mut frames);
    assert_eq!(engine.previewed_scenes(), vec![SceneId::new("other")]);
    drop(frames);
    drop(control);
    assert!(engine.previewed_scenes().is_empty());
}

/// A scene's stream costs the program stage nothing: a display only that scene shows
/// is not captured there (the preview captures it itself).
#[test]
fn a_scene_stream_captures_nothing() {
    let engine = Engine::new().with_cpu_compositor();
    let hub = engine.render_hub();
    let control = connected(&engine);
    let _frames = control.binary();
    let display = Edit::SourceCreate {
        source_id: SourceId::new("screen"),
        name: "Screen".into(),
        kind: SourceKind::Builtin {
            kind: "display".into(),
        },
        settings: Default::default(),
        duplicated_from: None,
    };
    let scenes = vec![
        create("main"),
        create("b"),
        display,
        add("b", "shown", "screen"),
    ];
    batch(&control, edits(scenes)).expect("scenes");
    assert!(engine.settle_sources(), "the display source was built");
    act(&control, subscribe_scene(1, "b")).expect("subscribed");
    assert_eq!(engine.previewed_scenes(), vec![SceneId::new("b")]);
    assert!(hub.load().shot().captures().is_empty());
}

/// The engine with the platform fakes, whose video devices `devices` shares, drawing
/// previews with a stand-in that sends, for each picture, the captures it had a frame
/// of.
fn recording_captures(
    devices: &fake::FakeVideoDevices,
) -> (Engine, mpsc::Receiver<Vec<CaptureSpec>>) {
    let clock = FakeClock::new(jstream_media::Timebase::MICROS);
    let mut backends = fake::backends(&clock).expect("fake backends");
    backends.video_devices = Arc::new(devices.clone());
    let (seen_tx, seen) = mpsc::channel();
    let draw: OpenPreviewDraw = Arc::new(move || {
        let seen = seen_tx.clone();
        let draw: PreviewDraw = Box::new(
            move |shot: &Shot, _: &LiveParams, captures: &dyn Captures, size, _, out| {
                let drawn: Vec<CaptureSpec> = shot
                    .captures()
                    .iter()
                    .filter(|key| captures.frame(key).is_some())
                    .map(|key| key.spec.clone())
                    .collect();
                let _ = seen.send(drawn);
                out.clear();
                out.resize(size.width() as usize * size.height() as usize * 4, 0);
                Ok(())
            },
        );
        Ok(draw)
    });
    let engine = Engine::new()
        .with_backends(backends)
        .with_cpu_compositor()
        .with_preview_draw(draw);
    (engine, seen)
}

/// A `display` source with its default settings: the primary display.
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

/// Reads `frames` until the pictures drawn so far had a frame of each of `wanted`.
fn until_drawn(
    frames: &mut BinaryStream,
    seen: &mpsc::Receiver<Vec<CaptureSpec>>,
    wanted: &[CaptureSpec],
) {
    let deadline = Instant::now() + WAIT;
    let mut drawn = Vec::new();
    loop {
        drawn.extend(seen.try_iter().flatten());
        if wanted.iter().all(|spec| drawn.contains(spec)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{wanted:?} never all reached the preview: {drawn:?}"
        );
        frame(frames);
    }
}

/// A display and a webcam the program shows are drawn into the preview with no output
/// running: the preview thread captures them itself, opening the webcam once.
#[test]
fn the_preview_captures_what_the_program_shows() {
    let devices = fake::FakeVideoDevices::new(FakeClock::new(jstream_media::Timebase::MICROS));
    let (engine, seen) = recording_captures(&devices);
    let control = connected(&engine);
    let mut frames = control.binary();
    act(&control, subscribe(1, Some(30), Vec::new())).expect("subscribed");
    let scene = vec![
        create("scn"),
        screen(),
        add("scn", "itm", "screen"),
        webcam(),
        add("scn", "cam", "webcam"),
    ];
    let rev = batch(&control, edits(scene)).expect("scene");
    frame_at(&mut frames, rev);
    until_drawn(
        &mut frames,
        &seen,
        &[CaptureSpec::Display(None), CaptureSpec::Device(None)],
    );
    assert_eq!(devices.open_streams(), 1, "the webcam opens once");

    // Once no stream shows it, the preview closes the webcam.
    act(
        &control,
        Call::Action {
            action: Action::PreviewUnsubscribe { channel: 1 },
        },
    )
    .expect("unsubscribed");
    let deadline = Instant::now() + WAIT;
    while devices.open_streams() > 0 {
        assert!(Instant::now() < deadline, "the webcam stays open");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A scene's stream (a projector of a scene off program) draws the display that scene
/// shows, captured by the preview, while the program stage captures nothing.
#[test]
fn a_scene_stream_draws_its_display() {
    let devices = fake::FakeVideoDevices::new(FakeClock::new(jstream_media::Timebase::MICROS));
    let (engine, seen) = recording_captures(&devices);
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut frames = control.binary();
    let scenes = vec![
        create("main"),
        create("b"),
        screen(),
        add("b", "shown", "screen"),
    ];
    batch(&control, edits(scenes)).expect("scenes");
    act(&control, subscribe_scene(1, "b")).expect("subscribed");
    until_drawn(&mut frames, &seen, &[CaptureSpec::Display(None)]);
    assert!(hub.load().shot().captures().is_empty());
}

/// [`subscribe`] to a source the collection doesn't hold, of `kind` with `settings`.
fn subscribe_draft(channel: u32, kind: &str, settings: serde_json::Value) -> Call {
    let settings = serde_json::from_value(settings).expect("settings");
    Call::Action {
        action: Action::PreviewSubscribe {
            channel,
            max_width: None,
            max_height: None,
            fps: Some(30),
            codecs: Vec::new(),
            scene_id: None,
            source: Some(PreviewDraft {
                kind: kind.into(),
                settings,
            }),
            source_id: None,
        },
    }
}

/// The Properties dialog's picture: a webcam that is not a source yet is drawn, captured
/// for the stream, and the collection is left as it was. Closing the stream closes it.
#[test]
fn a_draft_stream_shows_a_webcam_before_it_is_a_source() {
    let devices = fake::FakeVideoDevices::new(FakeClock::new(jstream_media::Timebase::MICROS));
    let (engine, seen) = recording_captures(&devices);
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut frames = control.binary();
    let before = engine.rev();
    act(
        &control,
        subscribe_draft(1, "webcam", serde_json::json!({})),
    )
    .expect("subscribed");
    until_drawn(&mut frames, &seen, &[CaptureSpec::Device(None)]);
    assert_eq!(devices.open_streams(), 1, "the draft opened the webcam");
    assert!(
        !hub.load()
            .shot()
            .captures()
            .iter()
            .any(|key| matches!(key.spec, CaptureSpec::Device(_))),
        "the program shows no webcam"
    );
    let after = engine.rev();
    assert_eq!(before, after, "a draft never reaches the collection");

    act(
        &control,
        Call::Action {
            action: Action::PreviewUnsubscribe { channel: 1 },
        },
    )
    .expect("unsubscribed");
    let deadline = Instant::now() + WAIT;
    while devices.open_streams() > 0 {
        assert!(Instant::now() < deadline, "the webcam stays open");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A draft of the webcam the program already shows shares its one open, and a draft
/// display is captured beside it.
#[test]
fn a_draft_shares_the_webcam_the_program_shows() {
    let devices = fake::FakeVideoDevices::new(FakeClock::new(jstream_media::Timebase::MICROS));
    let (engine, seen) = recording_captures(&devices);
    let control = connected(&engine);
    let mut frames = control.binary();
    let scene = vec![create("scn"), webcam(), add("scn", "cam", "webcam")];
    batch(&control, edits(scene)).expect("scene");
    act(&control, subscribe(1, Some(30), Vec::new())).expect("program");
    act(
        &control,
        subscribe_draft(2, "webcam", serde_json::json!({})),
    )
    .expect("draft");
    act(
        &control,
        subscribe_draft(3, "display", serde_json::json!({"show_cursor": false})),
    )
    .expect("display draft");
    until_drawn(
        &mut frames,
        &seen,
        &[CaptureSpec::Device(None), CaptureSpec::Display(None)],
    );
    assert_eq!(devices.open_streams(), 1, "the webcam opens once");
}

#[test]
fn drafts_are_checked() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let _frames = control.binary();
    let field = |error: ControlError| match error {
        ControlError::Protocol(WireError {
            error: ProtocolError::InvalidArgument { field, .. },
            ..
        }) => field,
        other => panic!("expected invalid_argument, got {other:?}"),
    };
    let not_capture = act(&control, subscribe_draft(1, "color", serde_json::json!({})))
        .expect_err("a color source captures nothing");
    assert_eq!(field(not_capture), "source.kind");
    let unknown =
        act(&control, subscribe_draft(1, "nope", serde_json::json!({}))).expect_err("no such kind");
    assert_eq!(field(unknown), "source.kind");
    let bad = act(
        &control,
        subscribe_draft(1, "webcam", serde_json::json!({"device": 5})),
    )
    .expect_err("a number for a device");
    assert_eq!(field(bad), "source.settings.device");
    let Call::Action {
        action: Action::PreviewSubscribe { source, .. },
    } = subscribe_draft(1, "webcam", serde_json::json!({}))
    else {
        unreachable!()
    };
    let both = act(
        &control,
        Call::Action {
            action: Action::PreviewSubscribe {
                channel: 1,
                max_width: None,
                max_height: None,
                fps: None,
                codecs: Vec::new(),
                scene_id: Some(SceneId::new("main")),
                source,
                source_id: None,
            },
        },
    )
    .expect_err("a scene and a source");
    assert_eq!(field(both), "source");
    // A refused draft leaves its channel free.
    act(
        &control,
        subscribe_draft(1, "window", serde_json::json!({})),
    )
    .expect("subscribed");
}

/// [`subscribe`] to the collection's source `source` alone, at 30 fps.
fn subscribe_source(channel: u32, source: &str) -> Call {
    Call::Action {
        action: Action::PreviewSubscribe {
            channel,
            max_width: None,
            max_height: None,
            fps: Some(30),
            codecs: Vec::new(),
            scene_id: None,
            source: None,
            source_id: Some(SourceId::new(source)),
        },
    }
}

/// A projector of a source: the stream draws a source no scene places, follows its
/// frames, and stops planning it once unsubscribed.
#[test]
fn a_source_stream_shows_the_source_alone() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    let rev = batch(&control, edits(vec![create("main"), webcam()])).expect("source");
    act(&control, subscribe_source(2, "webcam")).expect("subscribed");
    assert_eq!(engine.previewed_sources(), vec![SourceId::new("webcam")]);
    let first = frame_at(&mut frames, rev);
    assert_eq!(first.header.channel, 2);
    assert_eq!((first.header.width, first.header.height), (640, 360));
    // The program places nothing, so a new picture comes from the source alone.
    let first_pts = first.header.pts_us;
    engine
        .set_source_frame(&SourceId::new("webcam"), 2, 2, &[255; 16])
        .expect("frame");
    let changed = frame(&mut frames);
    assert!(changed.header.pts_us > first_pts);
    assert!(
        Duration::from_micros(u64::try_from(changed.header.pts_us - first_pts).expect("later"))
            < KEYFRAME_EVERY,
        "drawn only for a keyframe"
    );
    // The collection is as it was: no scene or item was added for the stream.
    let Ok(QueryResult::StateGet { scenes, .. }) = query(&control, Query::StateGet) else {
        panic!("state.get");
    };
    assert_eq!(scenes.len(), 1);
    act(
        &control,
        Call::Action {
            action: Action::PreviewUnsubscribe { channel: 2 },
        },
    )
    .expect("unsubscribed");
    assert!(engine.previewed_sources().is_empty());
}

/// Deleting a source ends its streams with an end frame that says why.
#[test]
fn deleting_the_source_ends_its_stream() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let mut frames = control.binary();
    batch(&control, edits(vec![create("main"), webcam()])).expect("source");
    act(&control, subscribe_source(3, "webcam")).expect("subscribed");
    frame(&mut frames);
    batch(
        &control,
        Batch::single(Edit::SourceDelete {
            source_id: SourceId::new("webcam"),
        }),
    )
    .expect("deleted");
    let deadline = Instant::now() + WAIT;
    let end = loop {
        let frame = frame(&mut frames);
        if frame.header.end {
            break frame;
        }
        assert!(Instant::now() < deadline, "no end frame");
    };
    assert_eq!(end.header.channel, 3);
    assert_eq!(&*end.payload, b"the source was deleted");
    assert!(engine.previewed_sources().is_empty());
}

/// A source's stream draws the display it captures, captured by the preview, while
/// the program stage captures nothing.
#[test]
fn a_source_stream_draws_its_display() {
    let devices = fake::FakeVideoDevices::new(FakeClock::new(jstream_media::Timebase::MICROS));
    let (engine, seen) = recording_captures(&devices);
    let hub = engine.render_hub();
    let control = connected(&engine);
    let mut frames = control.binary();
    batch(&control, edits(vec![create("main"), screen()])).expect("source");
    act(&control, subscribe_source(1, "screen")).expect("subscribed");
    until_drawn(&mut frames, &seen, &[CaptureSpec::Display(None)]);
    assert!(hub.load().shot().captures().is_empty());
}

#[test]
fn source_streams_are_checked() {
    let engine = Engine::new().with_cpu_compositor();
    let control = connected(&engine);
    let _frames = control.binary();
    batch(&control, edits(vec![create("main"), webcam()])).expect("source");
    let missing = act(&control, subscribe_source(1, "nowhere")).expect_err("no such source");
    assert_eq!(code(&missing), Some("not_found"));
    let both = act(
        &control,
        Call::Action {
            action: Action::PreviewSubscribe {
                channel: 1,
                max_width: None,
                max_height: None,
                fps: None,
                codecs: Vec::new(),
                scene_id: Some(SceneId::new("main")),
                source: None,
                source_id: Some(SourceId::new("webcam")),
            },
        },
    )
    .expect_err("a scene and a source");
    match both {
        ControlError::Protocol(WireError {
            error: ProtocolError::InvalidArgument { field, .. },
            ..
        }) => assert_eq!(field, "source_id"),
        other => panic!("expected invalid_argument, got {other:?}"),
    }
    // A refused subscription leaves its channel free.
    act(&control, subscribe_source(1, "webcam")).expect("subscribed");
}
