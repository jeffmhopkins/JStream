//! VA-API, loaded at run time (ADR-0060 §6, §8).
//!
//! Draft: [`probe_encoders`] opens each DRM render node through libva and lists the H.264
//! encode entry points its driver offers. Encode sessions are a follow-up.

/// The H.264 profiles JStream encodes, with libva's `VAProfile` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum H264Profile {
    /// `VAProfileH264Main`.
    Main = 6,
    /// `VAProfileH264High`, what RTMP ingest is usually fed.
    High = 7,
    /// `VAProfileH264ConstrainedBaseline`.
    ConstrainedBaseline = 13,
}

impl H264Profile {
    /// Every profile, in the order the probe prefers them.
    pub const ALL: [Self; 3] = [Self::High, Self::Main, Self::ConstrainedBaseline];
}

/// The encode entry points of libva's `VAEntrypoint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum VaEntrypoint {
    /// `VAEntrypointEncSlice`: the full encoder.
    EncSlice = 6,
    /// `VAEntrypointEncSliceLP`: the low-power encoder (Intel's fixed-function path).
    EncSliceLp = 8,
}

impl VaEntrypoint {
    /// The encode entry point to use out of `offered` (raw `VAEntrypoint` values): the
    /// full encoder if there is one, else the low-power one.
    pub fn choose(offered: &[i32]) -> Option<Self> {
        [Self::EncSlice, Self::EncSliceLp]
            .into_iter()
            .find(|e| offered.contains(&(*e as i32)))
    }
}

/// One H.264 encoder a VA-API driver offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaEncoder {
    /// The DRM render node, such as `/dev/dri/renderD128`.
    pub node: String,
    /// The driver's vendor string.
    pub vendor: String,
    /// The best profile it encodes.
    pub profile: H264Profile,
    /// The entry point it encodes through.
    pub entrypoint: VaEntrypoint,
}

#[cfg(target_os = "linux")]
pub use backend::probe_encoders;

#[cfg(target_os = "linux")]
mod backend {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;

    use jstream_platform::PlatformError;

    use super::{H264Profile, VaEncoder, VaEntrypoint};
    use crate::dl::SystemLibrary;

    type VaDisplay = *mut c_void;
    type GetDisplayDrm = unsafe extern "C" fn(fd: c_int) -> VaDisplay;
    type Initialize = unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> c_int;
    type Terminate = unsafe extern "C" fn(VaDisplay) -> c_int;
    type QueryVendorString = unsafe extern "C" fn(VaDisplay) -> *const c_char;
    type MaxNumEntrypoints = unsafe extern "C" fn(VaDisplay) -> c_int;
    type QueryConfigEntrypoints =
        unsafe extern "C" fn(VaDisplay, c_int, *mut c_int, *mut c_int) -> c_int;

    /// `VA_STATUS_SUCCESS`.
    const VA_OK: c_int = 0;

    /// The libva functions the probe calls, with the libraries that hold them.
    struct Va {
        get_display_drm: GetDisplayDrm,
        initialize: Initialize,
        terminate: Terminate,
        query_vendor_string: QueryVendorString,
        max_num_entrypoints: MaxNumEntrypoints,
        query_config_entrypoints: QueryConfigEntrypoints,
        _va: SystemLibrary,
        _drm: SystemLibrary,
    }

    impl Va {
        fn load() -> Result<Self, PlatformError> {
            let remedy = "install libva (Debian and Ubuntu: libva2 and libva-drm2; Fedora: \
                          libva) and your GPU's VA-API driver";
            let va = SystemLibrary::open("libva", &["libva.so.2"], remedy)?;
            let drm = SystemLibrary::open("libva-drm", &["libva-drm.so.2"], remedy)?;
            // SAFETY: each type alias above is the C declaration in <va/va.h> or
            // <va/va_drm.h> (`VADisplay` is `void *`; `VAStatus`, `VAProfile` and
            // `VAEntrypoint` are `int`-sized), and both libraries stay loaded in `Va`.
            unsafe {
                Ok(Self {
                    get_display_drm: drm.function("vaGetDisplayDRM")?,
                    initialize: va.function("vaInitialize")?,
                    terminate: va.function("vaTerminate")?,
                    query_vendor_string: va.function("vaQueryVendorString")?,
                    max_num_entrypoints: va.function("vaMaxNumEntrypoints")?,
                    query_config_entrypoints: va.function("vaQueryConfigEntrypoints")?,
                    _va: va,
                    _drm: drm,
                })
            }
        }
    }

    /// An initialised VA display on one render node. Terminated on drop, before the node
    /// is closed.
    struct Display<'a> {
        va: &'a Va,
        dpy: VaDisplay,
        // Dropped after `drop` below has terminated the display.
        _node: File,
    }

    impl<'a> Display<'a> {
        /// Opens `node` and initialises a display on it; `None` when the node can't be
        /// opened or no driver initialises on it.
        fn open(va: &'a Va, node: &str) -> Option<Self> {
            let file = OpenOptions::new().read(true).write(true).open(node).ok()?;
            // SAFETY: the fd is open for the display's lifetime (`_node`); libva only
            // stores it.
            let dpy = unsafe { (va.get_display_drm)(file.as_raw_fd()) };
            if dpy.is_null() {
                return None;
            }
            let (mut major, mut minor) = (0, 0);
            // SAFETY: `dpy` came from vaGetDisplayDRM; the out-pointers are valid.
            let status = unsafe { (va.initialize)(dpy, &mut major, &mut minor) };
            if status != VA_OK {
                // SAFETY: vaTerminate releases a display even when initialisation failed.
                unsafe { (va.terminate)(dpy) };
                return None;
            }
            Some(Self {
                va,
                dpy,
                _node: file,
            })
        }

        fn vendor(&self) -> String {
            // SAFETY: `dpy` is initialised; the result is a NUL-terminated string owned
            // by the driver for the display's lifetime, or null.
            let ptr = unsafe { (self.va.query_vendor_string)(self.dpy) };
            if ptr.is_null() {
                return String::new();
            }
            // SAFETY: non-null and NUL-terminated, and copied before the display drops.
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        }

        /// The raw entry points the driver offers for `profile`.
        fn entrypoints(&self, profile: H264Profile) -> Vec<i32> {
            // SAFETY: `dpy` is initialised.
            let max = unsafe { (self.va.max_num_entrypoints)(self.dpy) };
            let Ok(cap) = usize::try_from(max) else {
                return Vec::new();
            };
            let mut list = vec![0 as c_int; cap];
            let mut count: c_int = 0;
            // SAFETY: `list` holds `vaMaxNumEntrypoints` entries, the most libva writes;
            // `count` is a valid out-pointer.
            let status = unsafe {
                (self.va.query_config_entrypoints)(
                    self.dpy,
                    profile as c_int,
                    list.as_mut_ptr(),
                    &mut count,
                )
            };
            if status != VA_OK {
                // VA_STATUS_ERROR_UNSUPPORTED_PROFILE and the like: none for this profile.
                return Vec::new();
            }
            list.truncate(usize::try_from(count).unwrap_or(0).min(cap));
            list
        }
    }

    impl Drop for Display<'_> {
        fn drop(&mut self) {
            // SAFETY: `dpy` is initialised and terminated exactly once, here.
            unsafe { (self.va.terminate)(self.dpy) };
        }
    }

    /// The H.264 encoders every DRM render node's VA-API driver offers, one per node at
    /// most. Empty when there is no render node or no driver encodes H.264 (Fedora's
    /// stock Mesa, NVIDIA's driver).
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libva isn't installed.
    pub fn probe_encoders() -> Result<Vec<VaEncoder>, PlatformError> {
        let va = Va::load()?;
        let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("renderD"))
                    })
                    .filter_map(|p| p.to_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        nodes.sort();
        let mut found = Vec::new();
        for node in nodes {
            let Some(display) = Display::open(&va, &node) else {
                continue;
            };
            let best = H264Profile::ALL.into_iter().find_map(|profile| {
                VaEntrypoint::choose(&display.entrypoints(profile)).map(|e| (profile, e))
            });
            if let Some((profile, entrypoint)) = best {
                found.push(VaEncoder {
                    vendor: display.vendor(),
                    node,
                    profile,
                    entrypoint,
                });
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::{H264Profile, VaEntrypoint};

    #[test]
    fn values_match_libva() {
        assert_eq!(H264Profile::Main as i32, 6);
        assert_eq!(H264Profile::High as i32, 7);
        assert_eq!(H264Profile::ConstrainedBaseline as i32, 13);
        assert_eq!(VaEntrypoint::EncSlice as i32, 6);
        assert_eq!(VaEntrypoint::EncSliceLp as i32, 8);
    }

    #[test]
    fn the_full_encoder_wins_over_low_power() {
        assert_eq!(
            VaEntrypoint::choose(&[1, 6, 8]),
            Some(VaEntrypoint::EncSlice)
        );
        assert_eq!(
            VaEntrypoint::choose(&[1, 8]),
            Some(VaEntrypoint::EncSliceLp)
        );
        // Decode only (VAEntrypointVLD).
        assert_eq!(VaEntrypoint::choose(&[1]), None);
        assert_eq!(VaEntrypoint::choose(&[]), None);
    }

    /// Runs on any Linux machine: with no libva it reports the missing component, and
    /// with no GPU (CI) it finds no encoder.
    #[cfg(target_os = "linux")]
    #[test]
    fn probing_never_fails_without_a_gpu() {
        use jstream_platform::PlatformError;

        match super::probe_encoders() {
            Ok(encoders) => {
                for e in encoders {
                    assert!(e.node.starts_with("/dev/dri/renderD"));
                }
            }
            Err(PlatformError::MissingComponent { component, .. }) => {
                assert!(component.starts_with("libva"));
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
