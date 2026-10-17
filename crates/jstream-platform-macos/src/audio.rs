//! Audio through Core Audio (ADR-0063 §4).
//!
//! Devices and inputs go through the Core Audio HAL: `CoreAudioIo` lists every device
//! under its persistent UID, once per direction it has channels in, and records an input
//! with an IOProc on the device (`input`), at the device's own rate as interleaved `f32`.
//! Desktop and application audio will go through process taps, which need macOS 14.2
//! ([`Feature::ProcessTaps`]): the loopback targets are checked for that and otherwise
//! report `Unsupported`, as playing audio does, until they are built.
//!
//! `layout` holds the parts that make no system call, so they are tested on every
//! platform; `hal` reads the HAL's properties and `input` runs the streams.

#[cfg(target_os = "macos")]
mod hal;
#[cfg(target_os = "macos")]
mod input;
#[cfg(any(target_os = "macos", test))]
mod layout;

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

    use super::input::{self, Which};
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
            AudioInputTarget::DefaultInput => Ok(Which::Default),
            AudioInputTarget::Input(id) => Ok(Which::Device(id.clone())),
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

    /// Audio I/O through Core Audio (ADR-0063 §4): it lists devices and records inputs
    /// through the HAL. Desktop and application audio, which need process taps, are
    /// refused before 14.2 and report `Unsupported` after, as playing does, for now.
    ///
    /// Without the Microphone permission macOS records silence rather than failing.
    #[derive(Debug)]
    pub struct CoreAudioIo {
        version: MacosVersion,
    }

    impl CoreAudioIo {
        /// The backend, on a release at or above ADR-0063's floor.
        ///
        /// # Errors
        /// [`PlatformError::Unsupported`] below macOS 14.0.
        pub fn new() -> Result<Self, PlatformError> {
            let version = MacosVersion::current();
            check_floor(version)?;
            Ok(Self { version })
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
            input::format(&which(target)?)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            check_input(target, self.version)?;
            input::open(which(target)?, handler)
        }

        fn open_output(
            &self,
            _target: &AudioOutputTarget,
            _handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            Err(not_built("playing audio"))
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
    use jstream_platform::{AudioDirection, AudioInputTarget, AudioIo, DeviceId, PlatformError};

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
        AudioInputHandler, AudioInputTarget, AudioIo, AudioPacket, MediaClock, PlatformError,
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
