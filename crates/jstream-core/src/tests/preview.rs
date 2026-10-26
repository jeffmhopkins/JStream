//! Preview streams (U1, ADR-0045): `preview.subscribe` sends the program mix to the
//! session that asked, as binary frames on its channel, in H.264 when the platform has
//! an encoder and JPEG otherwise; a reader that falls behind loses frames up to a
//! keyframe, and the streams end with `preview.unsubscribe` or the session. A stream may
//! show one scene instead, planned for it alone, and ends when the scene is deleted.

use super::{add, batch, block_on, code, connected, create, edits, live, moved, webcam};
use crate::preview::{BINARY_BUFFER, KEYFRAME_EVERY};
use crate::{Engine, InProcessControl};
use jstream_control::{BinaryStream, Control, ControlError};
use jstream_platform::fake::{self, FakeClock};
use jstream_protocol::{
    Action, Batch, BinaryFrame, Call, Edit, PreviewCodec, ProtocolError, Rev, SceneId, SourceId,
    SourceKind, WireError,
};
use std::sync::Arc;
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

/// A scene's stream costs the program stage nothing: a display only that scene shows
/// is not captured, since a preview draws a capture as nothing.
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
