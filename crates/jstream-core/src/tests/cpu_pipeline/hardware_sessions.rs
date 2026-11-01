//! A full hardware session limit fails the start (ADR-0080 §3, #443 case 2): while any
//! output holds a hardware video session, a new session opens with `require`, and a
//! refusal is `hardware_session_limit` naming the holders, never a silent fallback to
//! software. `avoid` still opens software, and with no hardware session open `prefer`
//! falls back as before. A codec the machine lists no hardware encoder for isn't held
//! to `require`, so its failure stays `action_failed`. An output that is stopping still
//! holds its hardware session until it closes (#834). A new session that `prefer` opens
//! in software logs a warning naming the output and why (#443 case 1).

use super::super::{batch, block_on, connected, edits, output_call, warnings, warnings_with};
use super::{Fakes, TempDir, encoder_status, fakes, pump_until, streaming_and_recording};
use crate::runtime::cpu_draw;
use crate::{Engine, InProcessControl, RecordingPublish};
use jstream_control::Control;
use jstream_media::Packet;
use jstream_platform::fake::FakeCodecs;
use jstream_platform::{
    AudioEncoder, AudioEncoderConfig, CodecInfo, Codecs, HardwarePreference, PlatformError,
    VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig, VideoSurface,
};
use jstream_protocol::{
    Edit, EncoderProfile, EncoderProfileId, HardwarePreference as ProfileHardware, OutputId,
    OutputKind, OutputState, ProtocolError, RateControl, RecordContainer, VideoCodec,
};
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Codecs with a hardware video encoder that takes `cap` sessions at a time, as a GPU's
/// session limit does, and the fakes' software encoder besides. Under `prefer` a full
/// hardware encoder falls back to software at configure, as the platforms' do; under
/// `require` it refuses.
struct LimitedHardware {
    cap: usize,
    open: Arc<AtomicUsize>,
    /// Every hardware preference an open asked for, in order.
    asked: Arc<std::sync::Mutex<Vec<HardwarePreference>>>,
    /// Holds a hardware session's close while a test says so.
    linger: Arc<Linger>,
    /// Whether `list` names the hardware encoder. A machine with none lists none.
    listed: bool,
    /// Whether a hardware session refuses its first frame and falls back to software,
    /// as a Windows MFT under `prefer` can (#443 case 3).
    refuse_first: bool,
}

/// Keeps a closing hardware session open while held, as a teardown still releasing
/// it would, and tells the test when one is waiting.
#[derive(Default)]
struct Linger {
    state: std::sync::Mutex<LingerState>,
    changed: Condvar,
}

#[derive(Default)]
struct LingerState {
    held: bool,
    closing: bool,
}

impl Linger {
    fn hold(&self) {
        self.state.lock().expect("linger").held = true;
    }

    fn release(&self) {
        self.state.lock().expect("linger").held = false;
        self.changed.notify_all();
    }

    /// Whether a session started closing within ten seconds.
    fn wait_for_close(&self) -> bool {
        let state = self.state.lock().expect("linger");
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(10), |state| !state.closing)
            .expect("linger");
        state.closing
    }

    /// Called as a session closes: waits while held.
    fn close(&self) {
        let mut state = self.state.lock().expect("linger");
        state.closing = true;
        self.changed.notify_all();
        while state.held {
            state = self.changed.wait(state).expect("linger");
        }
    }
}

impl LimitedHardware {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            open: Arc::new(AtomicUsize::new(0)),
            asked: Arc::default(),
            linger: Arc::default(),
            listed: true,
            refuse_first: false,
        }
    }

    /// A machine with no hardware encoder: it lists none and opens none.
    fn absent() -> Self {
        Self {
            listed: false,
            ..Self::new(0)
        }
    }
}

/// A hardware session: the fake encoder under a hardware name, counted while open.
struct HardwareVideo {
    inner: Box<dyn VideoEncoder>,
    info: CodecInfo,
    /// Falls back to software at its first frame, which `info` then reports.
    refuse_first: bool,
    open: Arc<AtomicUsize>,
    linger: Arc<Linger>,
}

impl Drop for HardwareVideo {
    fn drop(&mut self) {
        self.linger.close();
        self.open.fetch_sub(1, Ordering::AcqRel);
    }
}

impl VideoEncoder for HardwareVideo {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        self.inner.can_accept()
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        if std::mem::take(&mut self.refuse_first) {
            self.info.name = "Fake software H.264 encoder".into();
            self.info.hardware = false;
        }
        self.inner.encode(frame)
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.inner.poll(out)
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.inner.finish(out)
    }
}

impl Codecs for LimitedHardware {
    fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
        let mut list = FakeCodecs.list()?;
        if !self.listed {
            return Ok(list);
        }
        let mut hardware = list
            .first()
            .cloned()
            .expect("the fakes list a video encoder");
        hardware.name = "Fake hardware H.264 encoder".into();
        hardware.hardware = true;
        list.push(hardware);
        Ok(list)
    }

    fn video_encoder(
        &self,
        config: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
        self.asked.lock().expect("asked").push(config.hardware);
        let software = VideoEncoderConfig {
            hardware: HardwarePreference::Avoid,
            ..*config
        };
        if config.hardware == HardwarePreference::Avoid {
            return FakeCodecs.video_encoder(&software);
        }
        let taken = self
            .open
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < self.cap).then_some(open + 1)
            })
            .is_ok();
        if !taken {
            return match config.hardware {
                HardwarePreference::Require => Err(PlatformError::Unsupported {
                    what: "another hardware encoder session".into(),
                }),
                _ => FakeCodecs.video_encoder(&software),
            };
        }
        let inner = match FakeCodecs.video_encoder(&software) {
            Ok(inner) => inner,
            Err(err) => {
                self.open.fetch_sub(1, Ordering::AcqRel);
                return Err(err);
            }
        };
        let mut info = inner.info().clone();
        info.name = "Fake hardware H.264 encoder".into();
        info.hardware = true;
        Ok(Box::new(HardwareVideo {
            inner,
            info,
            refuse_first: self.refuse_first,
            open: Arc::clone(&self.open),
            linger: Arc::clone(&self.linger),
        }))
    }

    fn audio_encoder(
        &self,
        config: &AudioEncoderConfig,
    ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
        FakeCodecs.audio_encoder(config)
    }

    fn video_decoder(
        &self,
        config: &VideoDecoderConfig,
    ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
        FakeCodecs.video_decoder(config)
    }
}

/// Two outputs on one engine: `yt` on the default profile (`prefer`), started, and
/// `rec`, recording on profile `hq`, whose rate differs so it needs a session of its
/// own, and what starting it answered. Dropping it stops both.
struct Both {
    /// What `output.start` for `rec` answered: the protocol error, if it failed.
    rec: Result<(), Option<ProtocolError>>,
    control: InProcessControl,
    _engine: Engine,
    /// The fake clock and audio, to move the pipeline on.
    fakes: Fakes,
    _dir: TempDir,
}

impl Both {
    /// `id`'s state, whether its video encoder is a hardware one, and the video and
    /// hardware sessions open.
    fn status(&self, id: &str) -> (OutputState, Option<bool>, u32, u32) {
        let (state, encoder, sessions, hardware) = encoder_status(&self.control, id);
        (state, encoder.map(|e| e.hardware), sessions, hardware)
    }
}

impl Drop for Both {
    fn drop(&mut self) {
        for id in ["rec", "yt"] {
            let _ = block_on(self.control.call(output_call(id, false)));
        }
    }
}

/// [`Both`] on `codecs`, with `hq`'s `hardware` as given, recording in a temp directory
/// called `name`.
fn start_both(name: &str, codecs: LimitedHardware, hardware: ProfileHardware) -> Both {
    start_both_on(name, codecs, hardware, VideoCodec::H264)
}

/// [`start_both`] with `hq` encoding `codec`.
fn start_both_on(
    name: &str,
    codecs: LimitedHardware,
    hardware: ProfileHardware,
    codec: VideoCodec,
) -> Both {
    let dir = TempDir::new(name);
    let path = dir.join("show.mp4");
    let mut fakes = fakes();
    fakes.backends.codecs = Arc::new(codecs);
    let publish = RecordingPublish::new();
    let (engine, control, _events) = streaming_and_recording(&fakes, cpu_draw(), &publish, &path);
    let mut profile = EncoderProfile::DEFAULT;
    profile.video.rate_control = RateControl::Cbr { kbps: 6000 };
    profile.video.hardware = hardware;
    profile.video.codec = codec;
    batch(
        &control,
        edits(vec![
            Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("hq"),
                name: "Recording".into(),
                profile,
            },
            Edit::OutputSetEncoder {
                output_id: OutputId::new("rec"),
                profile_id: Some(EncoderProfileId::new("hq")),
            },
        ]),
    )
    .expect("encoder edits");

    block_on(control.call(output_call("yt", true))).expect("yt start");
    let rec = block_on(control.call(output_call("rec", true)))
        .map(|_| ())
        .map_err(|err| err.protocol().cloned());
    Both {
        rec,
        control,
        _engine: engine,
        fakes,
        _dir: dir,
    }
}

/// With `yt` holding the one hardware session the encoder takes, starting `rec` on
/// `prefer` fails with `hardware_session_limit` naming `yt`, and asks the encoder for
/// `require`, not a software fallback. `yt` keeps streaming.
#[test]
fn a_full_hardware_session_limit_fails_the_start() {
    let codecs = LimitedHardware::new(1);
    let asked = Arc::clone(&codecs.asked);
    let both = start_both("hw-full", codecs, ProfileHardware::Prefer);

    let err = both
        .rec
        .clone()
        .expect_err("rec started past the session limit");
    assert_eq!(
        err.as_ref().map(ProtocolError::code),
        Some("hardware_session_limit"),
        "{err:?}"
    );
    match err {
        Some(ProtocolError::HardwareSessionLimit { outputs, reason }) => {
            assert_eq!(outputs, vec![OutputId::new("yt")]);
            assert!(
                reason.contains("another hardware encoder session"),
                "the encoder's refusal is kept: {reason}"
            );
        }
        other => panic!("not the session limit: {other:?}"),
    }
    assert_eq!(
        *asked.lock().expect("asked"),
        vec![HardwarePreference::Prefer, HardwarePreference::Require],
        "the second open must not be allowed to fall back"
    );
    assert_eq!(both.status("rec").0, OutputState::Failed);
    assert_eq!(both.status("yt"), (OutputState::Active, Some(true), 1, 1));
}

/// `require` on the second output is refused the same way.
#[test]
fn require_past_the_limit_is_the_session_limit_too() {
    let both = start_both(
        "hw-require",
        LimitedHardware::new(1),
        ProfileHardware::Require,
    );
    assert_eq!(
        both.rec
            .clone()
            .expect_err("rec started past the session limit")
            .as_ref()
            .map(ProtocolError::code),
        Some("hardware_session_limit")
    );
}

/// `avoid` opens the software encoder whatever hardware sessions are open.
#[test]
fn avoid_opens_software_beside_a_hardware_session() {
    let both = start_both("hw-avoid", LimitedHardware::new(1), ProfileHardware::Avoid);
    both.rec.clone().expect("rec on avoid");
    assert_eq!(both.status("rec"), (OutputState::Active, Some(false), 2, 1));
    assert_eq!(both.status("yt").1, Some(true));
}

/// Under the limit, the second output gets a hardware session of its own.
#[test]
fn a_second_hardware_session_opens_under_the_limit() {
    let both = start_both(
        "hw-second",
        LimitedHardware::new(2),
        ProfileHardware::Prefer,
    );
    both.rec.clone().expect("rec under the limit");
    assert_eq!(both.status("rec"), (OutputState::Active, Some(true), 2, 2));
}

/// With no hardware session open, `prefer` still falls back to software when hardware
/// refuses (ADR-0031, #443 case 1): `yt` gets no hardware session, so `rec` isn't held
/// to `require` either.
#[test]
fn prefer_falls_back_when_no_hardware_session_is_open() {
    let codecs = LimitedHardware::new(0);
    let asked = Arc::clone(&codecs.asked);
    let both = start_both("hw-none", codecs, ProfileHardware::Prefer);
    both.rec
        .clone()
        .expect("rec with no hardware sessions held");
    assert_eq!(both.status("yt"), (OutputState::Active, Some(false), 2, 0));
    assert_eq!(both.status("rec").1, Some(false));
    assert_eq!(
        *asked.lock().expect("asked"),
        vec![HardwarePreference::Prefer, HardwarePreference::Prefer]
    );
}

/// A held hardware H.264 session is no session limit for a codec the machine lists no
/// hardware encoder for (#814's HEVC here): `prefer` isn't held to `require`, and the
/// failure is `action_failed` with what the machine lacks, not `hardware_session_limit`.
#[test]
fn a_codec_without_hardware_is_no_session_limit() {
    let codecs = LimitedHardware::new(2);
    let asked = Arc::clone(&codecs.asked);
    let both = start_both_on("hw-hevc", codecs, ProfileHardware::Prefer, VideoCodec::Hevc);
    match both
        .rec
        .clone()
        .expect_err("the fakes have no HEVC encoder")
    {
        Some(ProtocolError::ActionFailed { reason }) => {
            assert!(reason.contains("no HEVC encoder"), "{reason}");
        }
        other => panic!("not action_failed: {other:?}"),
    }
    assert_eq!(
        *asked.lock().expect("asked"),
        vec![HardwarePreference::Prefer, HardwarePreference::Prefer],
        "the HEVC open keeps its own preference"
    );
    assert_eq!(both.status("yt"), (OutputState::Active, Some(true), 1, 1));
}

/// A start beside an output that is stopping, while that output's hardware session is
/// still closing and the program stage stays up for `rec`, counts the session as held
/// (#834): with the cap still full it fails as `hardware_session_limit` naming the
/// stopping output, rather than falling back to software.
#[test]
fn a_stopping_holder_still_holds_its_hardware_session() {
    let dir = TempDir::new("hw-stopping");
    let codecs = LimitedHardware::new(1);
    let linger = Arc::clone(&codecs.linger);
    let mut fakes = fakes();
    fakes.backends.codecs = Arc::new(codecs);
    let publish = RecordingPublish::new();
    let (engine, control, _events) =
        streaming_and_recording(&fakes, cpu_draw(), &publish, &dir.join("show.mp4"));
    let profile = |kbps, hardware| {
        let mut profile = EncoderProfile::DEFAULT;
        profile.video.rate_control = RateControl::Cbr { kbps };
        profile.video.hardware = hardware;
        profile
    };
    let encoder = |id: &str, profile| Edit::EncoderCreate {
        profile_id: EncoderProfileId::new(id),
        name: id.into(),
        profile,
    };
    let set = |output: &str, profile: &str| Edit::OutputSetEncoder {
        output_id: OutputId::new(output),
        profile_id: Some(EncoderProfileId::new(profile)),
    };
    let second = dir.join("second.mp4");
    batch(
        &control,
        edits(vec![
            encoder("soft", profile(4000, ProfileHardware::Avoid)),
            encoder("hq", profile(8000, ProfileHardware::Prefer)),
            Edit::OutputCreate {
                output_id: OutputId::new("rec2"),
                kind: OutputKind::Record {
                    path: second.to_str().expect("a UTF-8 temp path").into(),
                    container: RecordContainer::Mp4,
                },
            },
            set("rec", "soft"),
            set("rec2", "hq"),
        ]),
    )
    .expect("encoder edits");
    // `rec` keeps the program stage up on software; `yt` takes the one hardware session.
    block_on(control.call(output_call("rec", true))).expect("rec start");
    block_on(control.call(output_call("yt", true))).expect("yt start");
    assert_eq!(
        encoder_status(&control, "yt").1.map(|e| e.hardware),
        Some(true)
    );

    linger.hold();
    let stopper = connected(&engine);
    let stop = std::thread::spawn(move || block_on(stopper.call(output_call("yt", false))));
    let closing = linger.wait_for_close();
    let started = closing.then(|| block_on(control.call(output_call("rec2", true))));
    linger.release();
    stop.join().expect("the stop thread").expect("yt stop");
    assert!(closing, "yt's hardware session never started closing");

    match started.map(|started| started.map_err(|err| err.protocol().cloned())) {
        Some(Err(Some(ProtocolError::HardwareSessionLimit { outputs, .. }))) => {
            assert_eq!(outputs, vec![OutputId::new("yt")]);
        }
        other => panic!("rec2 wasn't refused as the session limit: {other:?}"),
    }
    assert_eq!(encoder_status(&control, "yt").0, OutputState::Stopped);
    assert_eq!(encoder_status(&control, "rec2").0, OutputState::Failed);
    let _ = block_on(control.call(output_call("rec", false)));
}

/// The warnings among `logged` that say a `prefer` encoder opened in software.
fn software_warnings(logged: &[String]) -> Vec<&String> {
    logged
        .iter()
        .filter(|line| line.contains("prefers hardware but opened in software"))
        .collect()
}

/// `prefer` with a hardware encoder listed but refusing at configure opens in software,
/// and logs one warning per new session naming its output and that hardware refused
/// (#443 case 1).
#[test]
fn a_refused_prefer_warns_naming_the_output() {
    let (both, logged) = warnings(|| {
        start_both(
            "hw-warn-refused",
            LimitedHardware::new(0),
            ProfileHardware::Prefer,
        )
    });
    both.rec.clone().expect("rec on software");
    let warned = software_warnings(&logged);
    assert_eq!(warned.len(), 2, "one per new session: {logged:?}");
    for (line, output) in warned.iter().zip(["yt", "rec"]) {
        assert!(line.contains(&format!("output_id={output} ")), "{line}");
        assert!(
            line.contains("the hardware H.264 encoder refused the session"),
            "{line}"
        );
    }
}

/// `prefer` on a machine with no hardware encoder warns with that reason.
#[test]
fn an_absent_hardware_encoder_warns_naming_the_output() {
    let (both, logged) = warnings(|| {
        start_both(
            "hw-warn-absent",
            LimitedHardware::absent(),
            ProfileHardware::Prefer,
        )
    });
    both.rec.clone().expect("rec on software");
    let warned = software_warnings(&logged);
    assert_eq!(warned.len(), 2, "{logged:?}");
    assert!(warned[0].contains("output_id=yt "), "{}", warned[0]);
    assert!(
        warned[0].contains("this machine has no hardware H.264 encoder"),
        "{}",
        warned[0]
    );
}

/// Hardware opened, or software chosen with `avoid`, warns of nothing.
#[test]
fn hardware_or_avoid_warns_of_nothing() {
    let (both, logged) = warnings(|| {
        start_both(
            "hw-warn-avoid",
            LimitedHardware::new(1),
            ProfileHardware::Avoid,
        )
    });
    both.rec.clone().expect("rec on avoid");
    assert_eq!(both.status("yt").1, Some(true), "yt opened hardware");
    assert_eq!(both.status("rec").1, Some(false));
    assert!(software_warnings(&logged).is_empty(), "{logged:?}");
}

/// A hardware encoder that refuses its first frame and falls back to software has each
/// output on it warn once, naming itself and both encoders, when its output thread gets
/// its first video packet (#965).
#[test]
fn a_first_frame_fallback_warns_naming_each_output() {
    let codecs = LimitedHardware {
        refuse_first: true,
        ..LimitedHardware::new(2)
    };
    let fallback = |line: &&String| line.contains("refused its first frame");
    let (both, logged) = warnings_with(|logged| {
        let both = start_both("hw-warn-first", codecs, ProfileHardware::Prefer);
        pump_until(&both.fakes.clock, &both.fakes.audio, || {
            logged().iter().filter(fallback).count() >= 2
        });
        both
    });
    both.rec.clone().expect("rec on hardware, then software");
    let warned: Vec<&String> = logged.iter().filter(fallback).collect();
    assert_eq!(warned.len(), 2, "one per output: {logged:?}");
    for output in ["yt", "rec"] {
        assert!(
            warned
                .iter()
                .any(|line| line.contains(&format!("output_id={output} "))),
            "{output}: {warned:?}"
        );
    }
    for line in &warned {
        assert!(
            line.contains("hardware=Fake hardware H.264 encoder "),
            "{line}"
        );
        assert!(
            line.contains("software=Fake software H.264 encoder "),
            "{line}"
        );
    }
    assert_eq!(
        both.status("yt").1,
        Some(false),
        "yt reports software after it"
    );
}
