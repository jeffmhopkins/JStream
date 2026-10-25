//! A full hardware session limit fails the start (ADR-0027 §3, #443 case 2): while any
//! output holds a hardware video session, a new session opens with `require`, and a
//! refusal is `hardware_session_limit` naming the holders, never a silent fallback to
//! software. `avoid` still opens software, and with no hardware session open `prefer`
//! falls back as before.

use super::super::{batch, block_on, edits, output_call};
use super::{TempDir, encoder_status, fakes, streaming_and_recording};
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
    OutputState, ProtocolError, RateControl,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Codecs with a hardware video encoder that takes `cap` sessions at a time, as a GPU's
/// session limit does, and the fakes' software encoder besides. Under `prefer` a full
/// hardware encoder falls back to software at configure, as the platforms' do; under
/// `require` it refuses.
struct LimitedHardware {
    cap: usize,
    open: Arc<AtomicUsize>,
    /// Every hardware preference an open asked for, in order.
    asked: Arc<std::sync::Mutex<Vec<HardwarePreference>>>,
}

impl LimitedHardware {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            open: Arc::new(AtomicUsize::new(0)),
            asked: Arc::default(),
        }
    }
}

/// A hardware session: the fake encoder under a hardware name, counted while open.
struct HardwareVideo {
    inner: Box<dyn VideoEncoder>,
    info: CodecInfo,
    open: Arc<AtomicUsize>,
}

impl Drop for HardwareVideo {
    fn drop(&mut self) {
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
            open: Arc::clone(&self.open),
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
    let dir = TempDir::new(name);
    let path = dir.join("show.mp4");
    let mut fakes = fakes();
    fakes.backends.codecs = Arc::new(codecs);
    let publish = RecordingPublish::new();
    let (engine, control, _events) = streaming_and_recording(&fakes, cpu_draw(), &publish, &path);
    let mut profile = EncoderProfile::DEFAULT;
    profile.video.rate_control = RateControl::Cbr { kbps: 6000 };
    profile.video.hardware = hardware;
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
