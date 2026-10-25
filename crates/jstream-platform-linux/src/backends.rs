//! The Linux backend set: the backends this crate has, built together, with the audio
//! backend chosen as ADR-0060 §4 says (PipeWire when a daemon runs, else ALSA) and
//! handed to V4L2 so webcams are paired with microphones that backend opens.

#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), test))]
use jstream_platform::PlatformError;

/// The audio backend a [`LinuxBackends`] chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioBackend {
    /// PipeWire: a daemon answered. Inputs, outputs, desktop and application audio.
    PipeWire,
    /// ALSA, the fallback: PipeWire couldn't be used. Inputs and outputs only.
    Alsa,
}

/// Picks the audio backend: `pipewire`'s when it succeeds, else `alsa`'s, with the
/// reason PipeWire wasn't used. When neither can be used, a missing component naming
/// both reasons.
#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), test))]
pub(crate) fn choose<A>(
    pipewire: impl FnOnce() -> Result<A, PlatformError>,
    alsa: impl FnOnce() -> Result<A, PlatformError>,
) -> Result<(A, AudioBackend, Option<PlatformError>), PlatformError> {
    let why = match pipewire() {
        Ok(audio) => return Ok((audio, AudioBackend::PipeWire, None)),
        Err(e) => e,
    };
    match alsa() {
        Ok(audio) => Ok((audio, AudioBackend::Alsa, Some(why))),
        Err(alsa_err) => Err(PlatformError::MissingComponent {
            component: "audio",
            remedy: format!(
                "neither PipeWire nor ALSA could be used. PipeWire: {why}. ALSA: {alsa_err}. \
                 Start the PipeWire daemon (systemctl --user start pipewire), or install \
                 ALSA's library (libasound2)"
            ),
        }),
    }
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub use set::LinuxBackends;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod set {
    use std::path::PathBuf;
    use std::sync::Arc;

    use jstream_platform::{AudioIo, Backends, GpuInterop, MediaDecoders, PlatformError};

    use super::{AudioBackend, choose};
    use crate::{
        AlsaAudio, MonotonicClock, PairedAudio, PipeWireAudio, PortalCapture, Session, V4l2Devices,
        VaapiCodecs,
    };

    /// The backends this crate has, built together for one process: the media clock,
    /// portal screen capture, V4L2 video devices, the audio backend and VA-API codecs.
    ///
    /// The audio backend is PipeWire when its library loads and a daemon answers a
    /// device listing, and ALSA otherwise (ADR-0060 §4); [`audio_backend`] says which,
    /// and [`pipewire_unavailable`] why PipeWire wasn't used. The video devices are
    /// paired with that same backend's inputs, so a webcam's paired microphone is an ID
    /// [`audio`] opens. The choice is made once, when the set is built: a daemon that
    /// starts later isn't picked up until the next one.
    ///
    /// The engine's [`Backends`] also needs GPU interop and media-file decoders, which
    /// [`into_backends`](Self::into_backends) takes from the host: `LinuxGpu` is this
    /// crate's GPU interop, and it has no media-file decoders yet.
    ///
    /// [`audio_backend`]: Self::audio_backend
    /// [`pipewire_unavailable`]: Self::pipewire_unavailable
    /// [`audio`]: Self::audio
    #[derive(Debug)]
    pub struct LinuxBackends {
        /// The `CLOCK_MONOTONIC` media clock.
        pub clock: Arc<MonotonicClock>,
        /// Display and window capture through the ScreenCast portal.
        pub capture: Arc<PortalCapture>,
        /// Webcams and capture cards, paired with [`audio`](Self::audio)'s inputs.
        pub video_devices: Arc<V4l2Devices>,
        /// The chosen audio backend.
        pub audio: Arc<dyn PairedAudio>,
        /// Which one [`audio`](Self::audio) is.
        pub audio_backend: AudioBackend,
        /// Why PipeWire wasn't used, when [`audio_backend`](Self::audio_backend) is ALSA:
        /// worth a log line, since desktop and application audio need PipeWire.
        pub pipewire_unavailable: Option<PlatformError>,
        /// VA-API H.264 encoding, the software H.264 and AAC encoders.
        pub codecs: Arc<VaapiCodecs>,
    }

    impl LinuxBackends {
        /// Builds the set for `session`. Capture keeps its restore tokens in `token_dir`
        /// when given (see [`PortalCapture::with_token_dir`]), else in memory. Choosing
        /// the audio backend connects to the PipeWire daemon once, to list its devices;
        /// the VA-API codecs probe the render nodes.
        ///
        /// # Errors
        /// [`PlatformError::MissingComponent`] when neither PipeWire nor ALSA can be
        /// used, naming why for each, and [`PlatformError::Os`] when the media clock
        /// can't be read.
        pub fn new(session: Session, token_dir: Option<PathBuf>) -> Result<Self, PlatformError> {
            let clock = Arc::new(MonotonicClock::new()?);
            let (audio, audio_backend, pipewire_unavailable) = choose(
                || {
                    let pw = PipeWireAudio::new()?;
                    pw.devices()?;
                    Ok(Arc::new(pw) as Arc<dyn PairedAudio>)
                },
                || Ok(Arc::new(AlsaAudio::new()?) as Arc<dyn PairedAudio>),
            )?;
            let capture = Arc::new(match token_dir {
                Some(dir) => PortalCapture::with_token_dir(session, dir),
                None => PortalCapture::new(session),
            });
            Ok(Self {
                clock,
                capture,
                video_devices: Arc::new(V4l2Devices::with_audio(Arc::clone(&audio))),
                audio,
                audio_backend,
                pipewire_unavailable,
                codecs: Arc::new(VaapiCodecs::new()),
            })
        }

        /// The engine's backend set, with the GPU interop and media-file decoders the
        /// host supplies. Linux has no virtual camera (ADR-0060 §9), so `vcam` is `None`.
        pub fn into_backends(
            self,
            gpu: Arc<dyn GpuInterop>,
            media: Arc<dyn MediaDecoders>,
        ) -> Backends {
            let audio: Arc<dyn AudioIo> = self.audio;
            Backends {
                clock: self.clock,
                capture: self.capture,
                video_devices: self.video_devices,
                audio,
                codecs: self.codecs,
                gpu,
                media,
                vcam: None,
            }
        }
    }

    #[cfg(all(test, feature = "hw-tests"))]
    mod hw_tests {
        use super::*;

        /// On a machine with a PipeWire daemon (`XDG_RUNTIME_DIR` naming its socket),
        /// the set picks PipeWire; without one, ALSA, saying why. Either way the video
        /// devices list (none here) and the audio backend lists its devices.
        #[test]
        #[ignore = "needs PipeWire's daemon or ALSA's library"]
        fn picks_pipewire_when_a_daemon_runs_else_alsa() {
            let set = LinuxBackends::new(Session::from_env(), None).expect("backends");
            let daemon = std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|d| std::path::Path::new(&d).join("pipewire-0").exists());
            if daemon {
                assert_eq!(set.audio_backend, AudioBackend::PipeWire);
                assert!(set.pipewire_unavailable.is_none());
            } else {
                assert_eq!(set.audio_backend, AudioBackend::Alsa);
                assert!(set.pipewire_unavailable.is_some());
            }
            set.audio.devices().expect("audio devices");
            set.audio.card_inputs().expect("card inputs");
            jstream_platform::VideoDevices::devices(&*set.video_devices).expect("video devices");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(component: &'static str) -> PlatformError {
        PlatformError::MissingComponent {
            component,
            remedy: format!("start {component}"),
        }
    }

    #[test]
    fn pipewire_wins_when_it_answers() {
        let (audio, backend, why) =
            choose(|| Ok("pw"), || panic!("ALSA isn't tried")).expect("chosen");
        assert_eq!((audio, backend, why), ("pw", AudioBackend::PipeWire, None));
    }

    #[test]
    fn alsa_is_the_fallback_and_keeps_pipewires_reason() {
        let (audio, backend, why) =
            choose(|| Err(missing("PipeWire")), || Ok("alsa")).expect("chosen");
        assert_eq!(
            (audio, backend, why),
            ("alsa", AudioBackend::Alsa, Some(missing("PipeWire")))
        );
    }

    #[test]
    fn neither_is_a_missing_component_naming_both() {
        let err = choose::<()>(|| Err(missing("PipeWire")), || Err(missing("ALSA")))
            .expect_err("neither");
        let PlatformError::MissingComponent { component, remedy } = err else {
            panic!("not a missing component: {err:?}");
        };
        assert_eq!(component, "audio");
        assert!(remedy.contains("start PipeWire") && remedy.contains("start ALSA"));
    }
}
