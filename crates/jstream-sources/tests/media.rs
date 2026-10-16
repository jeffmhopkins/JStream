//! The `media` kind on the platform's fake media files (R-TEST-004): frames on the
//! clock, looping, speed, restarting when shown, live updates, its audio handed on to a
//! mixer input, and its errors.
//!
//! Decoding runs on the source's own thread, so a test waits for it to catch up before
//! it reads which frame is shown ([`shown`]).

use jstream_media::{AudioFormat, ChannelLayout, FrameSize, MediaTime, SampleFormat, Timebase};
use jstream_platform::fake::{FakeAudio, FakeClip, FakeClock, FakeMediaDecoders};
use jstream_platform::{AudioInputHandler, AudioPacket, AudioStream, MediaClock, PlatformError};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Limits, MediaAudio, Registry, Source, SourceError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
        audio: None,
    }
}

/// [`clip`] with sound: 400 ms of 48 kHz stereo in 20 ms blocks.
fn with_sound() -> FakeClip {
    FakeClip {
        audio: Some(FakeAudio {
            format: AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
                .expect("a format"),
            block_frames: 960,
            blocks: 20,
        }),
        ..clip()
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

/// The `file` parameter takes the audio and video files the platform plays, `.m4a`
/// among them (#428), and refuses others.
#[test]
fn the_file_parameter_takes_media_extensions() {
    let registry = Registry::builtin();
    let file = registry
        .descriptor("media")
        .and_then(|kind| kind.param("file"))
        .expect("the file parameter");
    let asset = |path: &str| {
        Reference::Asset {
            root: AssetRoot::Collection,
            path: path.into(),
        }
        .to_value()
    };
    for path in ["clip.mp4", "song.m4a", "song.mp3", "song.flac"] {
        assert_eq!(file.check(&asset(path)), Ok(()), "{path}");
    }
    assert!(file.check(&asset("notes.txt")).is_err());
}

#[test]
fn files_it_cant_play_are_errors() {
    let (dir, fake) = setup("errors", clip());
    std::fs::write(dir.join("other.mp4"), b"not one the fake knows").expect("a file");
    std::fs::write(dir.join("empty.mp4"), b"no streams").expect("a file");
    fake.add(
        dir.join("empty.mp4").canonicalize().expect("the file"),
        FakeClip {
            frames: 0,
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
        err("empty.mp4"),
        SourceError::Decode { reason, .. } if reason.contains("no video or audio")
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

/// One packet a [`Recorder`] was handed.
#[derive(Clone, Debug, PartialEq)]
struct Got {
    /// Its stamp, in ms (rounded down).
    ms: i64,
    position: u64,
    frames: usize,
    /// Its first sample: which fake block it came from.
    level: f32,
    discontinuity: bool,
}

/// A mixer input's stand-in: records every packet, and whether it was dropped.
#[derive(Clone, Default)]
struct Recorder {
    got: Arc<Mutex<Vec<Got>>>,
    dropped: Arc<AtomicBool>,
}

impl Recorder {
    fn got(&self) -> Vec<Got> {
        self.got.lock().expect("the record").clone()
    }

    /// Waits until `done` holds of the packets, or fails after five seconds.
    fn wait(&self, what: &str, done: impl Fn(&[Got]) -> bool) -> Vec<Got> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let got = self.got();
            if done(&got) {
                return got;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {got:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct Handler(Recorder);

impl AudioInputHandler for Handler {
    fn on_packet(&mut self, packet: &AudioPacket<'_>) {
        let ms = packet
            .time
            .expect("a stamp")
            .rescale(Timebase::MILLIS, jstream_media::Rounding::Down)
            .expect("a time")
            .ticks();
        self.0.got.lock().expect("the record").push(Got {
            ms,
            position: packet.device_position,
            frames: packet.samples.len() / 2,
            level: packet.samples[0],
            discontinuity: packet.flags.discontinuity,
        });
    }

    fn on_error(&mut self, _: &PlatformError) {}
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

/// The source's audio, opened on a recorder, with the clock it is stamped on (at 0, the
/// source's time zero).
fn open_audio(source: &dyn Source) -> (MediaAudio, Box<dyn AudioStream>, Recorder, FakeClock) {
    let audio = source.audio().expect("the file has sound");
    let clock = FakeClock::new(Timebase::MICROS);
    let recorder = Recorder::default();
    let stream = audio
        .open(
            Box::new(Handler(recorder.clone())),
            Arc::new(clock.clone()),
            MediaTime::zero(Timebase::MICROS),
        )
        .expect("a stream");
    (audio, stream, recorder, clock)
}

/// Moves the clock to `t` ms and shows the frame then, as the program mix does.
fn at(source: &mut Box<dyn Source>, clock: &FakeClock, t: i64) {
    let now = clock.now().ticks();
    clock.advance(u64::try_from(t * 1000 - now).expect("forward"));
    let _ = shown(source, t);
}

#[test]
fn audio_is_handed_on_ahead_of_the_timeline_while_heard() {
    let (dir, fake) = setup("audio", with_sound());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 1.0, true),
    )
    .expect("a source");
    let (audio, stream, recorder, clock) = open_audio(source.as_ref());
    assert_eq!(audio.format().sample_rate(), 48_000);
    assert_eq!(stream.format(), audio.format());
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(recorder.got(), [], "nothing until the program shows it");

    audio.set_audible(true);
    // 100 ms ahead of the timeline: ten 10 ms packets, two from each 20 ms block, each
    // stamped where the timeline reaches it, one after another.
    let got = recorder.wait("100 ms of audio", |got| got.len() >= 10);
    let expected: Vec<Got> = (0..10)
        .map(|i| Got {
            ms: i * 10,
            position: u64::try_from(i).expect("an index") * 480,
            frames: 480,
            level: FakeAudio::level(u32::try_from(i / 2).expect("an index")),
            discontinuity: false,
        })
        .collect();
    assert_eq!(got[..10], expected[..]);
    // As the clock runs, the rest follows, up to the last packet at 390 ms.
    for t in (40..300).step_by(40) {
        at(&mut source, &clock, t);
        recorder.wait("audio 100 ms ahead", |got| {
            got.last().is_some_and(|g| g.ms >= (t + 90).min(390))
        });
    }
    at(&mut source, &clock, 300);
    let got = recorder.wait("the whole file", |got| {
        got.last().is_some_and(|g| g.ms == 390)
    });
    assert_eq!(got.len(), 40, "{got:?}");
    assert!(got.iter().all(|g| !g.discontinuity), "{got:?}");

    drop(stream);
    assert!(
        recorder.dropped.load(Ordering::SeqCst),
        "the handler goes with the stream"
    );
}

#[test]
fn a_restart_starts_the_audio_again_as_a_discontinuity() {
    let (dir, fake) = setup("audio-restart", with_sound());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), true, 1.0, true),
    )
    .expect("a source");
    let (audio, _stream, recorder, clock) = open_audio(source.as_ref());
    audio.set_audible(true);
    recorder.wait("the first packets", |got| got.len() >= 10);
    at(&mut source, &clock, 200);
    recorder.wait("audio up to 300 ms", |got| got.iter().any(|g| g.ms == 290));
    // Shown again at 250 ms: the file starts over there.
    source.show(ms(250));
    let got = recorder.wait("the start again", |got| {
        got.iter()
            .any(|g| g.ms == 250 && g.level == FakeAudio::level(0))
    });
    let again = got
        .iter()
        .position(|g| g.ms == 250 && g.level == FakeAudio::level(0))
        .expect("the restart");
    assert!(got[again].discontinuity, "{:?}", got[again]);
    let before = &got[again - 1];
    assert_eq!(
        got[again].position,
        before.position + 480,
        "positions run on"
    );
}

#[test]
fn audio_at_another_speed_is_silent() {
    let (dir, fake) = setup("audio-speed", with_sound());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 2.0, true),
    )
    .expect("a source");
    let (audio, _stream, recorder, clock) = open_audio(source.as_ref());
    audio.set_audible(true);
    for t in (20..=200).step_by(20) {
        at(&mut source, &clock, t);
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(recorder.got(), []);
    // Back to normal speed (and looping, as the first play is over), it is heard from
    // where the timeline is: 200 ms at double speed is 400 ms, the second play's start.
    source
        .update(&settings(Some("clip.mp4"), true, 1.0, true))
        .expect("live settings");
    let got = recorder.wait("audio again", |got| !got.is_empty());
    assert_eq!(
        (got[0].ms, got[0].level),
        (200, FakeAudio::level(0)),
        "{got:?}"
    );
}

#[test]
fn a_file_with_only_audio_plays_it_and_loops_seamlessly() {
    let (dir, fake) = setup(
        "audio-only",
        FakeClip {
            frames: 0,
            ..with_sound()
        },
    );
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), true, 1.0, true),
    )
    .expect("a source");
    let frame = source.frame(ms(0)).expect("a frame");
    assert_eq!(frame.info().size, FrameSize::new(1, 1).expect("a size"));
    assert_eq!(source.next_change(ms(0)), None, "nothing to see changes");
    let (audio, _stream, recorder, clock) = open_audio(source.as_ref());
    audio.set_audible(true);
    for t in (50..=500).step_by(50) {
        clock.advance(50_000);
        recorder.wait("audio up to the clock", |got| {
            got.last().is_some_and(|g| g.ms >= t + 80)
        });
    }
    let got = recorder.got();
    // Past 400 ms the file plays again, continuing the stamps and positions.
    let seam = got
        .iter()
        .position(|g| g.ms == 400)
        .expect("the second play");
    assert_eq!(got[seam].level, FakeAudio::level(0));
    assert!(!got[seam].discontinuity, "{:?}", got[seam]);
    for pair in got.windows(2) {
        assert_eq!(pair[1].ms - pair[0].ms, 10, "{pair:?}");
        assert_eq!(pair[1].position, pair[0].position + 480);
    }
    // Long unheard, it skips ahead rather than decoding through what it missed.
    audio.set_audible(false);
    let seeks = fake.stats().seeks;
    clock.advance(60_000_000);
    audio.set_audible(true);
    let got = recorder.wait("audio a minute on", |got| {
        got.last().is_some_and(|g| g.ms >= 60_500)
    });
    assert!(fake.stats().seeks > seeks, "it sought ahead");
    let resumed = got.iter().find(|g| g.ms >= 60_500).expect("a packet");
    assert!(resumed.ms < 60_700, "{resumed:?}");
}

/// #446: a decoder that hands over a second of sound at once (as an MP3 file did on
/// Windows) is still handed on only shortly ahead of the timeline. Handed on whole, its
/// end overwrote its start in the mixer input's ring before it was heard, so each
/// second played only its last fifth.
#[test]
fn long_decoded_blocks_are_handed_on_only_shortly_ahead() {
    let (dir, fake) = setup(
        "audio-long-blocks",
        FakeClip {
            frames: 0,
            audio: Some(FakeAudio {
                format: AudioFormat::new(
                    48_000,
                    ChannelLayout::Stereo,
                    SampleFormat::F32Interleaved,
                )
                .expect("a format"),
                block_frames: 48_000,
                blocks: 3,
            }),
            ..clip()
        },
    );
    let source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 1.0, true),
    )
    .expect("a source");
    let (audio, _stream, recorder, clock) = open_audio(source.as_ref());
    audio.set_audible(true);
    recorder.wait("100 ms of audio", |got| got.len() >= 10);
    // A few more ticks of the feeder, with the clock still at 0.
    std::thread::sleep(Duration::from_millis(50));
    let got = recorder.got();
    assert!(
        got.iter().all(|g| g.ms <= 100),
        "nothing more than 100 ms ahead: {:?}",
        got.last()
    );
    for t in (50..=1_500).step_by(50) {
        clock.advance(50_000);
        recorder.wait("audio up to the clock", |got| {
            got.last().is_some_and(|g| g.ms >= t + 90)
        });
        let ahead = recorder.got().last().map_or(0, |g| g.ms);
        assert!(ahead <= t + 100, "{ahead} ms handed on at {t} ms");
    }
    let got = recorder.got();
    // Every packet, across the blocks' seam at 1 s, follows the last one.
    for pair in got.windows(2) {
        assert_eq!(pair[1].ms - pair[0].ms, 10, "{pair:?}");
        assert_eq!(pair[1].position, pair[0].position + 480);
        assert!(!pair[1].discontinuity, "{pair:?}");
    }
    let seam = got.iter().position(|g| g.ms == 1_000).expect("the seam");
    assert_eq!(
        (got[seam - 1].level, got[seam].level),
        (FakeAudio::level(0), FakeAudio::level(1))
    );
}

/// [`clip`] with sound that outlasts the picture: 3 s of 48 kHz stereo.
fn with_long_sound() -> FakeClip {
    let mut clip = with_sound();
    if let Some(audio) = &mut clip.audio {
        audio.blocks = 150;
    }
    clip
}

#[test]
fn sound_that_outlasts_the_picture_plays_on_under_the_last_frame() {
    let (dir, fake) = setup("audio-tail", with_long_sound());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), false, 1.0, true),
    )
    .expect("a source");
    let (audio, _stream, recorder, clock) = open_audio(source.as_ref());
    audio.set_audible(true);
    let seeks = fake.stats().seeks;
    for t in (20..=2900).step_by(20) {
        at(&mut source, &clock, t);
        recorder.wait("audio 100 ms ahead", |got| {
            got.last().is_some_and(|g| g.ms >= (t + 90).min(2990))
        });
    }
    // The last frame stays up, and nothing changes until something else does.
    assert_eq!(shown(&mut source, 2900), 9);
    assert_eq!(source.next_change(ms(2900)), None);
    // No catch-up seek: the sound runs straight on to its end.
    assert_eq!(fake.stats().seeks, seeks, "it never sought");
    let got = recorder.got();
    assert_eq!(got.len(), 300, "{:?}", got.last());
    assert!(got.iter().all(|g| !g.discontinuity));
    for pair in got.windows(2) {
        assert_eq!(pair[1].ms - pair[0].ms, 10, "{pair:?}");
    }
}

#[test]
fn a_looping_file_shows_its_first_frame_again_once_the_sound_ends() {
    let (dir, fake) = setup("audio-tail-loop", with_long_sound());
    let mut source = create(
        &ctx(&dir, &fake),
        &settings(Some("clip.mp4"), true, 1.0, true),
    )
    .expect("a source");
    let (audio, _stream, recorder, clock) = open_audio(source.as_ref());
    audio.set_audible(true);
    let seeks = fake.stats().seeks;
    for t in (20..=1500).step_by(20) {
        at(&mut source, &clock, t);
        recorder.wait("audio 100 ms ahead", |got| {
            got.last().is_some_and(|g| g.ms >= t + 90)
        });
    }
    // Under the last frame, the next change is when the queued sound runs out, not
    // every few milliseconds.
    assert_eq!(shown(&mut source, 1500), 9);
    let next = source
        .next_change(ms(1500))
        .expect("a looping file goes on");
    assert!(next > ms(1600), "{next:?}");
    for t in (1520..=3000).step_by(20) {
        at(&mut source, &clock, t);
        recorder.wait("audio 100 ms ahead", |got| {
            got.last().is_some_and(|g| g.ms >= t + 90)
        });
    }
    // The second play starts at 3 s, with its first frame.
    assert_eq!(shown(&mut source, 3000), 0);
    assert_eq!(fake.stats().seeks, seeks + 1, "only the loop's seek");
}
