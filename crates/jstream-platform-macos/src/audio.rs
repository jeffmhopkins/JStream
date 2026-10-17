//! Audio through Core Audio (ADR-0063 §4).
//!
//! Inputs and outputs will go through the Core Audio HAL, and desktop and application
//! audio through process taps, which need macOS 14.2 ([`Feature::ProcessTaps`]). For now,
//! [`CoreAudioIo`] checks that requirement for the loopback targets and reports
//! `Unsupported` for everything else.

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

    use super::check_input;
    use crate::{Feature, MacosVersion, check_floor};

    fn not_built(what: &str) -> PlatformError {
        PlatformError::Unsupported {
            what: format!("{what} through Core Audio (not built yet, ADR-0063)"),
        }
    }

    /// Audio I/O through Core Audio (ADR-0063 §4). For now it reads the running release,
    /// refuses loopback before 14.2, and reports `Unsupported` for the rest.
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
            Err(not_built("listing audio devices"))
        }

        fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
            Feature::ProcessTaps.check(self.version)?;
            Err(not_built("listing applications playing audio"))
        }

        fn input_format(&self, target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
            check_input(target, self.version)?;
            Err(not_built("querying an input's format"))
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            _handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            check_input(target, self.version)?;
            Err(not_built("recording audio"))
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
