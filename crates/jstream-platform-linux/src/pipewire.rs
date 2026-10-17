//! PipeWire, loaded at run time (ADR-0060 §4, §8).
//!
//! [`PipeWire`] loads libpipewire and checks its version against ADR-0060's floor. The
//! video stream the ScreenCast portal hands over is [`stream`]. [`PipeWireAudio`] is the
//! `AudioIo` backend built on it: it lists devices and applications from the registry
//! ([`graph`]) and records and plays through audio streams ([`audio`]): input, output,
//! a sink's monitor for desktop audio and an application's stream for application
//! audio. The C API they share is [`ffi`].

/// The oldest PipeWire ADR-0060 supports: 1.0.
pub const PIPEWIRE_FLOOR: (u32, u32, u32) = (1, 0, 0);

/// Whether a PipeWire version string (such as `1.0.5`, as `pw_get_library_version`
/// returns it) is at least [`PIPEWIRE_FLOOR`]. A string that doesn't parse is not.
pub fn pipewire_version_supported(version: &str) -> bool {
    let mut parts = version.trim().split('.').map(|p| {
        // Tolerate a suffix on the last part, such as `1.2.0-rc1`.
        let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u32>().ok()
    });
    let (Some(Some(major)), minor, micro) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let minor = minor.flatten().unwrap_or(0);
    let micro = micro.flatten().unwrap_or(0);
    (major, minor, micro) >= PIPEWIRE_FLOOR
}

#[cfg(target_os = "linux")]
pub use backend::{PipeWire, PipeWireAudio};

#[cfg(target_os = "linux")]
mod audio;
/// Platform tests against a PipeWire daemon (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because they record and play on the user's default devices. Null
/// nodes are enough; see the file.
#[cfg(all(test, target_os = "linux", feature = "hw-tests"))]
mod audio_tests;
#[cfg(target_os = "linux")]
mod ffi;
#[cfg(any(target_os = "linux", test))]
mod graph;
#[cfg(target_os = "linux")]
pub(crate) mod stream;

#[cfg(target_os = "linux")]
mod backend {
    use std::ffi::{CStr, c_char};

    use jstream_platform::media::AudioFormat;
    use jstream_platform::{
        AudioDeviceInfo, AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler,
        AudioOutputTarget, AudioProcess, AudioStream, PlatformError,
    };

    use super::audio::{Connection, Open, PwAudioStream};
    use super::{PIPEWIRE_FLOOR, pipewire_version_supported};
    use crate::dl::SystemLibrary;

    type GetLibraryVersion = unsafe extern "C" fn() -> *const c_char;

    /// libpipewire, loaded and at least [`PIPEWIRE_FLOOR`].
    #[derive(Debug)]
    pub struct PipeWire {
        version: String,
        // Keeps the library loaded while the stream code holds its functions.
        lib: SystemLibrary,
    }

    impl PipeWire {
        /// Loads `libpipewire-0.3.so.0` and checks its version.
        ///
        /// # Errors
        /// [`PlatformError::MissingComponent`] when the library is missing or older than
        /// the floor.
        pub fn load() -> Result<Self, PlatformError> {
            let lib = SystemLibrary::open(
                "PipeWire",
                &["libpipewire-0.3.so.0"],
                "install PipeWire's client library (Debian and Ubuntu: libpipewire-0.3-0t64 \
                 or libpipewire-0.3-0; Fedora: pipewire-libs)",
            )?;
            // SAFETY: `const char *pw_get_library_version(void)` in <pipewire/version.h>.
            let get: GetLibraryVersion = unsafe { lib.function("pw_get_library_version") }?;
            // SAFETY: it takes no arguments, may be called before `pw_init`, and returns
            // a static NUL-terminated string (or null, checked below); `lib` is alive.
            let ptr = unsafe { get() };
            let version = if ptr.is_null() {
                String::new()
            } else {
                // SAFETY: non-null, NUL-terminated and static for the library's lifetime.
                unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            if !pipewire_version_supported(&version) {
                let (a, b, c) = PIPEWIRE_FLOOR;
                return Err(PlatformError::MissingComponent {
                    component: "PipeWire",
                    remedy: format!(
                        "PipeWire {a}.{b}.{c} or later is needed, and {version:?} is installed; \
                         update it"
                    ),
                });
            }
            Ok(Self { version, lib })
        }

        /// The library's version, as it reports it.
        pub fn version(&self) -> &str {
            &self.version
        }

        /// The loaded library, for the stream code's function table.
        pub(crate) fn library(&self) -> &SystemLibrary {
            &self.lib
        }
    }

    /// Audio I/O through PipeWire (ADR-0060 §4), on the user's PipeWire daemon.
    ///
    /// Each call reads the registry on a new connection, and each stream has its own
    /// connection and thread. A stream asks for interleaved `f32` at the graph's clock
    /// rate (`default.clock.rate`, usually 48 kHz) in the device's channel layout (mono,
    /// stereo or 5.1, from the device node's info; any other count, or a default with no
    /// device yet, as stereo), and PipeWire's stream adapter converts to it. Desktop audio records the default sink's monitor, and
    /// application audio the process's first playback stream.
    #[derive(Debug)]
    pub struct PipeWireAudio {
        pub(super) pw: PipeWire,
    }

    impl PipeWireAudio {
        /// Loads PipeWire. It doesn't connect to the daemon until a call needs it.
        ///
        /// # Errors
        /// As for [`PipeWire::load`].
        pub fn new() -> Result<Self, PlatformError> {
            Ok(Self {
                pw: PipeWire::load()?,
            })
        }

        fn connect(&self) -> Result<Connection, PlatformError> {
            Connection::open(self.pw.library())
        }
    }

    /// The file name of `pid`'s executable from `/proc`, else its `comm`; `None` when
    /// neither can be read (another user's process, or a pid from another namespace).
    fn exe_name(pid: u32) -> Option<String> {
        let dir = std::path::PathBuf::from(format!("/proc/{pid}"));
        std::fs::read_link(dir.join("exe"))
            .ok()
            .and_then(|p| p.file_name()?.to_str().map(str::to_owned))
            .or_else(|| {
                let comm = std::fs::read_to_string(dir.join("comm")).ok()?;
                Some(comm.trim_end().to_owned()).filter(|c| !c.is_empty())
            })
    }

    impl AudioIo for PipeWireAudio {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            Ok(self.connect()?.graph().devices())
        }

        fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
            Ok(self.connect()?.graph().processes(exe_name))
        }

        fn input_format(&self, target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
            self.connect()?.graph().input(target)?.format()
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            let conn = self.connect()?;
            let plan = conn.graph().input(target)?;
            Ok(Box::new(PwAudioStream::open(
                conn,
                &plan,
                Open::Input(handler),
            )?))
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            let conn = self.connect()?;
            let plan = conn.graph().output(target)?;
            Ok(Box::new(PwAudioStream::open(
                conn,
                &plan,
                Open::Output(handler),
            )?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pipewire_version_supported;

    #[test]
    fn versions_compare_against_the_floor() {
        assert!(pipewire_version_supported("1.0.0"));
        assert!(pipewire_version_supported("1.0.5"));
        assert!(pipewire_version_supported("1.4.2"));
        assert!(pipewire_version_supported("1.2.0-rc1"));
        assert!(pipewire_version_supported("2"));
        assert!(!pipewire_version_supported("0.3.85"));
        assert!(!pipewire_version_supported(""));
        assert!(!pipewire_version_supported("pipewire"));
    }

    /// Loads the library when it is installed, and otherwise checks the error names a
    /// package to install. Either way it runs without a PipeWire daemon.
    #[cfg(target_os = "linux")]
    #[test]
    fn loading_reports_a_version_or_a_remedy() {
        use jstream_platform::PlatformError;

        match super::PipeWire::load() {
            Ok(pw) => assert!(pipewire_version_supported(pw.version())),
            Err(PlatformError::MissingComponent { component, remedy }) => {
                assert_eq!(component, "PipeWire");
                assert!(remedy.contains("PipeWire") || remedy.contains("libpipewire"));
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    /// No daemon on the socket is a missing component whose remedy starts one. Only a
    /// socket name nothing listens on is tried, so no daemon is ever reached.
    #[cfg(target_os = "linux")]
    #[test]
    fn no_daemon_is_a_missing_component_with_a_remedy() {
        use jstream_platform::PlatformError;

        let Ok(pw) = super::PipeWire::load() else {
            return;
        };
        let missing =
            super::audio::Connection::open_to(pw.library(), Some(c"jstream-test-no-such-daemon"));
        match missing {
            Err(PlatformError::MissingComponent { component, remedy }) => {
                assert_eq!(component, "PipeWire daemon");
                assert!(
                    remedy.contains("systemctl --user start pipewire"),
                    "{remedy}"
                );
            }
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("connected to a daemon that doesn't exist"),
        }
    }
}
