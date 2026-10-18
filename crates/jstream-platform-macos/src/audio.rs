//! Audio through Core Audio (ADR-0063 §4).
//!
//! Devices, inputs and outputs go through the Core Audio HAL: `CoreAudioIo` lists every
//! device under its persistent UID, once per direction it has channels in, and records an
//! input or plays on an output with an IOProc on the device (`stream`), at the device's
//! own rate as interleaved `f32`. Desktop and application audio will go through process
//! taps, which need macOS 14.2 ([`Feature::ProcessTaps`]): the loopback targets are
//! checked for that and otherwise report `Unsupported` until they are built.
//!
//! `layout` and `permission` hold the parts that make no system call, so they are tested
//! on every platform; `hal` reads the HAL's properties and `stream` runs the streams.

#[cfg(target_os = "macos")]
mod hal;
#[cfg(any(target_os = "macos", test))]
mod layout;
#[cfg(any(target_os = "macos", test))]
mod permission;
#[cfg(target_os = "macos")]
mod stream;

use jstream_platform::{AudioInputTarget, PlatformError};

use crate::{Feature, MacosVersion};

/// The release feature `target` needs beyond the floor, if any: process taps for desktop
/// and application audio (ADR-0063 §4), nothing for a microphone or line input.
pub fn feature_for_input(target: &AudioInputTarget) -> Option<Feature> {
    match target {
        AudioInputTarget::DefaultOutputLoopback
        | AudioInputTarget::OutputLoopback(_)
        | AudioInputTarget::ProcessLoopback { .. } => Some(Feature::ProcessTaps),
        _ => None,
    }
}

/// Checks that `version` can record `target`.
///
/// # Errors
/// [`PlatformError::MissingComponent`], with the update to install, for desktop or
/// application audio before macOS 14.2.
pub fn check_input(target: &AudioInputTarget, version: MacosVersion) -> Result<(), PlatformError> {
    feature_for_input(target).map_or(Ok(()), |f| f.check(version))
}

#[cfg(target_os = "macos")]
pub use backend::CoreAudioIo;

#[cfg(target_os = "macos")]
mod backend {
    use jstream_platform::media::AudioFormat;
    use jstream_platform::{
        AudioDeviceInfo, AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler,
        AudioOutputTarget, AudioProcess, AudioStream, PlatformError,
    };

    use std::sync::atomic::{AtomicBool, Ordering};

    use jstream_platform::AudioDirection;

    use super::permission::{self, MicStep};
    use super::stream::{self, Which};
    use super::{check_input, hal};
    use crate::{Feature, MacosVersion, check_floor};

    fn not_built(what: &str) -> PlatformError {
        PlatformError::Unsupported {
            what: format!("{what} through Core Audio (not built yet, ADR-0063)"),
        }
    }

    /// The device an input target records, for the targets the HAL serves.
    fn which(target: &AudioInputTarget) -> Result<Which, PlatformError> {
        match target {
            AudioInputTarget::DefaultInput => Ok(Which {
                direction: AudioDirection::Input,
                device: None,
            }),
            AudioInputTarget::Input(id) => Ok(Which {
                direction: AudioDirection::Input,
                device: Some(id.clone()),
            }),
            AudioInputTarget::DefaultOutputLoopback
            | AudioInputTarget::OutputLoopback(_)
            | AudioInputTarget::ProcessLoopback { .. } => {
                Err(not_built("recording desktop or application audio"))
            }
            other => Err(PlatformError::Unsupported {
                what: format!("audio input target {other:?}"),
            }),
        }
    }

    /// The device an output target plays on.
    fn which_output(target: &AudioOutputTarget) -> Result<Which, PlatformError> {
        let device = match target {
            AudioOutputTarget::DefaultOutput => None,
            AudioOutputTarget::Output(id) => Some(id.clone()),
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("audio output target {other:?}"),
                });
            }
        };
        Ok(Which {
            direction: AudioDirection::Output,
            device,
        })
    }

    /// Audio I/O through Core Audio (ADR-0063 §4): it lists devices, records inputs and
    /// plays on outputs through the HAL. Desktop and application audio, which need
    /// process taps, are refused before 14.2 and report `Unsupported` after, for now.
    ///
    /// Recording an input needs the Microphone permission: without it macOS would record
    /// silence, so opening one checks it first and asks macOS to prompt once.
    #[derive(Debug)]
    pub struct CoreAudioIo {
        version: MacosVersion,
        /// Whether this backend already asked macOS to prompt for the Microphone permission.
        asked_microphone: AtomicBool,
    }

    impl CoreAudioIo {
        /// The backend, on a release at or above ADR-0063's floor.
        ///
        /// # Errors
        /// [`PlatformError::Unsupported`] below macOS 14.0.
        pub fn new() -> Result<Self, PlatformError> {
            let version = MacosVersion::current();
            check_floor(version)?;
            Ok(Self {
                version,
                asked_microphone: AtomicBool::new(false),
            })
        }

        /// Checks the Microphone permission before recording an input, asking macOS to
        /// prompt the first time it isn't determined. `open_input` calls it once the
        /// device is planned, so an unknown device never prompts.
        fn check_microphone(&self) -> Result<(), PlatformError> {
            let status = permission::authorization_status();
            match permission::mic_step(status, self.asked_microphone.load(Ordering::SeqCst)) {
                MicStep::Proceed => Ok(()),
                MicStep::Refuse => Err(permission::microphone_denied(status, false)),
                MicStep::Ask => {
                    self.asked_microphone.store(true, Ordering::SeqCst);
                    permission::request_access();
                    Err(permission::microphone_denied(status, true))
                }
            }
        }
    }

    impl AudioIo for CoreAudioIo {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            hal::list()
        }

        fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
            Feature::ProcessTaps.check(self.version)?;
            Err(not_built("listing applications playing audio"))
        }

        fn input_format(&self, target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
            check_input(target, self.version)?;
            stream::format(&which(target)?)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            check_input(target, self.version)?;
            let which = which(target)?;
            // An unknown or unusable device is reported as such, before any prompt.
            stream::format(&which)?;
            self.check_microphone()?;
            stream::open_input(which, handler)
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            // Playing needs no permission.
            stream::open_output(which_output(target)?, handler)
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::{AudioInputTarget, DeviceId, PlatformError};

    use super::{check_input, feature_for_input};
    use crate::{Feature, MacosVersion};

    #[test]
    fn loopback_needs_process_taps() {
        assert_eq!(feature_for_input(&AudioInputTarget::DefaultInput), None);
        assert_eq!(
            feature_for_input(&AudioInputTarget::DefaultOutputLoopback),
            Some(Feature::ProcessTaps)
        );
        let sonoma_1 = MacosVersion::new(14, 1, 0);
        assert!(check_input(&AudioInputTarget::DefaultInput, sonoma_1).is_ok());
        assert!(matches!(
            check_input(&AudioInputTarget::DefaultOutputLoopback, sonoma_1),
            Err(PlatformError::MissingComponent { .. })
        ));
        let output = AudioInputTarget::OutputLoopback(DeviceId::new("BuiltInSpeakerDevice"));
        assert!(check_input(&output, MacosVersion::new(14, 2, 0)).is_ok());
    }
}

/// Against the Mac's own audio system, with no permission needed, so they run in the
/// `macos` CI job; a runner may have no audio devices at all.
#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use jstream_platform::{
        AudioDirection, AudioInputTarget, AudioIo, AudioOutputHandler, AudioOutputTarget,
        AudioRenderBuffer, DeviceId, PlatformError,
    };

    use super::CoreAudioIo;

    #[test]
    fn lists_devices_with_at_most_one_default_each() {
        let devices = CoreAudioIo::new()
            .expect("backend")
            .devices()
            .expect("devices");
        for dir in [AudioDirection::Input, AudioDirection::Output] {
            let of_dir: Vec<_> = devices.iter().filter(|d| d.direction == dir).collect();
            assert!(
                of_dir.iter().filter(|d| d.is_default).count() <= 1,
                "{dir:?}"
            );
            for d in &of_dir {
                assert!(!d.id.as_str().is_empty(), "{d:?}");
            }
        }
    }

    #[test]
    fn every_listed_input_has_a_format() {
        let audio = CoreAudioIo::new().expect("backend");
        for d in audio.devices().expect("devices") {
            if d.direction == AudioDirection::Input {
                let target = AudioInputTarget::Input(d.id.clone());
                match audio.input_format(&target) {
                    Ok(f) => assert!(f.sample_rate() > 0, "{d:?}"),
                    // A device whose virtual format isn't f32 is refused, not misread.
                    Err(PlatformError::Unsupported { .. }) => {}
                    Err(e) => panic!("{d:?}: {e}"),
                }
            }
        }
    }

    #[test]
    fn reads_the_microphone_status_without_prompting() {
        let status = super::permission::authorization_status();
        assert!((0..=3).contains(&status), "{status}");
    }

    #[test]
    fn an_unknown_output_is_not_found() {
        struct Silence;
        impl AudioOutputHandler for Silence {
            fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
                buffer.samples.fill(0.0);
            }
            fn on_error(&mut self, _error: &PlatformError) {}
        }
        let audio = CoreAudioIo::new().expect("backend");
        let target = AudioOutputTarget::Output(DeviceId::new("no-such-jstream-device"));
        assert!(matches!(
            audio.open_output(&target, Box::new(Silence)),
            Err(PlatformError::NotFound { .. })
        ));
    }

    #[test]
    fn an_unknown_input_is_not_found() {
        let audio = CoreAudioIo::new().expect("backend");
        let target = AudioInputTarget::Input(DeviceId::new("no-such-jstream-device"));
        assert!(matches!(
            audio.input_format(&target),
            Err(PlatformError::NotFound { .. })
        ));
    }

    #[test]
    fn loopback_is_not_built_yet() {
        let audio = CoreAudioIo::new().expect("backend");
        assert!(matches!(
            audio.input_format(&AudioInputTarget::DefaultOutputLoopback),
            Err(PlatformError::Unsupported { .. } | PlatformError::MissingComponent { .. })
        ));
    }
}

/// Recording a real microphone: needs an input device and the Microphone permission for
/// the app running the tests, so it runs by hand on a Mac:
/// `cargo test -p jstream-platform-macos --features hw-tests -- --ignored`.
#[cfg(all(test, target_os = "macos", feature = "hw-tests"))]
mod hw_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use jstream_platform::{
        AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler, AudioOutputTarget,
        AudioPacket, AudioRenderBuffer, MediaClock, PlatformError,
    };

    use super::CoreAudioIo;
    use crate::MachClock;

    /// Counts frames and checks that positions and times only move forward.
    struct Count {
        channels: usize,
        frames: Arc<AtomicU64>,
        last_time: Arc<AtomicU64>,
        next_pos: u64,
        backwards: Arc<AtomicBool>,
    }

    impl AudioInputHandler for Count {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            let frames = (packet.samples.len() / self.channels) as u64;
            if packet.device_position < self.next_pos {
                self.backwards.store(true, Ordering::Relaxed);
            }
            self.next_pos = packet.device_position + frames;
            self.frames.fetch_add(frames, Ordering::Relaxed);
            if let Some(t) = packet.time {
                let t = t.ticks().cast_unsigned();
                if t < self.last_time.swap(t, Ordering::Relaxed) {
                    self.backwards.store(true, Ordering::Relaxed);
                }
            }
        }

        fn on_error(&mut self, _error: &PlatformError) {}
    }

    /// Plays a quiet tone and counts the frames it was asked for.
    struct Tone {
        channels: usize,
        phase: f32,
        frames: Arc<AtomicU64>,
        next_pos: u64,
        backwards: Arc<AtomicBool>,
    }

    impl AudioOutputHandler for Tone {
        fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
            if buffer.device_position < self.next_pos {
                self.backwards.store(true, Ordering::Relaxed);
            }
            for frame in buffer.samples.chunks_exact_mut(self.channels) {
                // A quiet 440 Hz tone at 48 kHz, close enough at other rates.
                frame.fill(0.05 * (self.phase * std::f32::consts::TAU).sin());
                self.phase = (self.phase + 440.0 / 48_000.0).fract();
            }
            let frames = (buffer.samples.len() / self.channels) as u64;
            self.next_pos = buffer.device_position + frames;
            self.frames.fetch_add(frames, Ordering::Relaxed);
        }

        fn on_error(&mut self, _error: &PlatformError) {}
    }

    #[test]
    #[ignore = "needs speakers and plays a tone"]
    fn plays_on_the_default_output() {
        let audio = CoreAudioIo::new().expect("backend");
        let expected = super::stream::format(&super::stream::Which {
            direction: jstream_platform::AudioDirection::Output,
            device: None,
        })
        .expect("output format");
        let frames = Arc::new(AtomicU64::new(0));
        let backwards = Arc::new(AtomicBool::new(false));
        let stream = audio
            .open_output(
                &AudioOutputTarget::DefaultOutput,
                Box::new(Tone {
                    channels: expected.channels(),
                    phase: 0.0,
                    frames: Arc::clone(&frames),
                    next_pos: 0,
                    backwards: Arc::clone(&backwards),
                }),
            )
            .expect("open");
        assert_eq!(stream.format(), expected);
        std::thread::sleep(Duration::from_secs(1));
        drop(stream);
        let got = frames.load(Ordering::Relaxed);
        let rate = u64::from(expected.sample_rate());
        assert!(got > rate / 2, "{got} frames in a second at {rate} Hz");
        assert!(!backwards.load(Ordering::Relaxed));
    }

    #[test]
    #[ignore = "needs a microphone and the Microphone permission"]
    fn records_the_default_input_on_the_media_clock() {
        let audio = CoreAudioIo::new().expect("backend");
        let target = AudioInputTarget::DefaultInput;
        let format = audio.input_format(&target).expect("input format");
        let frames = Arc::new(AtomicU64::new(0));
        let last_time = Arc::new(AtomicU64::new(0));
        let backwards = Arc::new(AtomicBool::new(false));
        let stream = audio
            .open_input(
                &target,
                Box::new(Count {
                    channels: format.channels(),
                    frames: Arc::clone(&frames),
                    last_time: Arc::clone(&last_time),
                    next_pos: 0,
                    backwards: Arc::clone(&backwards),
                }),
            )
            .expect("open");
        assert_eq!(stream.format(), format);
        std::thread::sleep(Duration::from_secs(1));
        drop(stream);
        let got = frames.load(Ordering::Relaxed);
        let rate = u64::from(format.sample_rate());
        assert!(got > rate / 2, "{got} frames in a second at {rate} Hz");
        assert!(!backwards.load(Ordering::Relaxed));
        let now = MachClock::new()
            .expect("clock")
            .now()
            .ticks()
            .cast_unsigned();
        let last = last_time.load(Ordering::Relaxed);
        // The last packet was recorded within the last second, on the same clock.
        assert!(last <= now && now - last < 10_000_000, "{last} vs {now}");
    }
}
