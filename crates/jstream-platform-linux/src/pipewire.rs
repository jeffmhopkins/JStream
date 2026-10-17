//! PipeWire, loaded at run time (ADR-0060 §4, §8).
//!
//! Draft: [`PipeWire`] loads libpipewire and checks its version against ADR-0060's floor,
//! and [`PipeWireAudio`] is the `AudioIo` backend built on it. Its streams (input,
//! output, a sink's monitor for desktop audio and an application's stream for
//! application audio) report `Unsupported` until the stream code lands.

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
mod backend {
    use std::ffi::{CStr, c_char};

    use jstream_platform::media::AudioFormat;
    use jstream_platform::{
        AudioDeviceInfo, AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler,
        AudioOutputTarget, AudioProcess, AudioStream, PlatformError,
    };

    use super::{PIPEWIRE_FLOOR, pipewire_version_supported};
    use crate::dl::SystemLibrary;

    type GetLibraryVersion = unsafe extern "C" fn() -> *const c_char;

    /// libpipewire, loaded and at least [`PIPEWIRE_FLOOR`].
    #[derive(Debug)]
    pub struct PipeWire {
        version: String,
        // Keeps the library loaded while the stream code holds its functions.
        _lib: SystemLibrary,
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
            Ok(Self { version, _lib: lib })
        }

        /// The library's version, as it reports it.
        pub fn version(&self) -> &str {
            &self.version
        }
    }

    /// Audio I/O through PipeWire (ADR-0060 §4). Draft: it loads PipeWire; listing
    /// devices and opening streams report `Unsupported` until the stream code lands.
    #[derive(Debug)]
    pub struct PipeWireAudio {
        _pw: PipeWire,
    }

    impl PipeWireAudio {
        /// Loads PipeWire.
        ///
        /// # Errors
        /// As for [`PipeWire::load`].
        pub fn new() -> Result<Self, PlatformError> {
            Ok(Self {
                _pw: PipeWire::load()?,
            })
        }
    }

    fn not_built(what: &str) -> PlatformError {
        PlatformError::Unsupported {
            what: format!("{what} through PipeWire (not built yet, ADR-0060)"),
        }
    }

    impl AudioIo for PipeWireAudio {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            Err(not_built("listing audio devices"))
        }

        fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
            Err(not_built("listing applications playing audio"))
        }

        fn input_format(&self, _target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
            Err(not_built("querying an input's format"))
        }

        fn open_input(
            &self,
            _target: &AudioInputTarget,
            _handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
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
}
