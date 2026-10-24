//! Detects a Windows without Media Foundation, such as an N or KN edition without the
//! Media Feature Pack, so the host fails with an error the user can act on instead of an
//! unexplained `MFStartup` failure (ADR-0061's Consequences name this gap).
//!
//! The probe loads `mfplat.dll` from System32 and looks for `MFStartup`; when that fails,
//! the edition (the registry's `EditionID`) picks the advice. The decision is plain data
//! in, error out, so it is tested on every platform.

use jstream_platform::PlatformError;

/// The component the check reports.
const COMPONENT: &str = "Media Foundation";

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Probe {
    /// `mfplat.dll` loads from System32 and exports `MFStartup`.
    pub(crate) media_foundation: bool,
    /// The registry's `EditionID`, such as `Professional` or `ProfessionalN`, if it
    /// could be read.
    pub(crate) edition: Option<String>,
}

/// Which kind of Windows is missing Media Foundation, which decides the advice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// An N or KN edition: the Media Feature Pack adds it.
    NEdition,
    /// Windows Server: the "Server Media Foundation" feature adds it.
    Server,
    /// Unknown: name both.
    Other,
}

fn kind(edition: Option<&str>) -> Kind {
    match edition {
        // Edition IDs of N and KN editions end in a capital N ("CoreN", "ProfessionalN",
        // "EnterpriseSN", "ProfessionalKN"); none of the others do ("Education" ends in
        // a lowercase n).
        Some(e) if e.ends_with('N') => Kind::NEdition,
        Some(e) if e.starts_with("Server") => Kind::Server,
        _ => Kind::Other,
    }
}

/// Turns a probe into the check's result: `Ok` when Media Foundation is there, else a
/// [`PlatformError::MissingComponent`] that says what to install.
pub(crate) fn decide(probe: &Probe) -> Result<(), PlatformError> {
    if probe.media_foundation {
        return Ok(());
    }
    let edition = probe.edition.as_deref();
    let remedy = match kind(edition) {
        Kind::NEdition => format!(
            "this is an N edition of Windows ({}), which ships without it. Install the \
             Media Feature Pack (Settings > System > Optional features > View features > \
             Media Feature Pack), restart, and start JStream again.",
            edition.unwrap_or_default()
        ),
        Kind::Server => format!(
            "Windows Server ({}) ships without it. Add the \"Server Media Foundation\" \
             feature (Install-WindowsFeature Server-Media-Foundation), restart, and start \
             JStream again.",
            edition.unwrap_or_default()
        ),
        Kind::Other => "Media Foundation (mfplat.dll) could not be loaded from System32. On an N edition of Windows, \
                        install the Media Feature Pack (Settings > System > Optional \
                        features); on Windows Server, add the \"Server Media Foundation\" \
                        feature. Then restart and start JStream again."
            .to_owned(),
    };
    Err(PlatformError::MissingComponent {
        component: COMPONENT,
        remedy,
    })
}

#[cfg(windows)]
pub use os::check_media_foundation;

#[cfg(windows)]
mod os {
    use jstream_platform::PlatformError;
    use windows::Win32::Foundation::{ERROR_SUCCESS, FreeLibrary, HMODULE};
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{s, w};

    use super::{Probe, decide};

    /// Checks that Media Foundation is installed. Call it before creating the backends:
    /// on a Windows N or KN edition without the Media Feature Pack (or a Windows Server
    /// without the Server Media Foundation feature) it returns
    /// [`PlatformError::MissingComponent`], whose message says what to install.
    ///
    /// The check itself never imports Media Foundation. A binary that links the codecs
    /// must delay-load `mfplat.dll`, or Windows refuses to start it on such a machine
    /// before this can run (crate README › Media Foundation check).
    pub fn check_media_foundation() -> Result<(), PlatformError> {
        decide(&Probe {
            media_foundation: media_foundation_loads(),
            edition: edition_id(),
        })
    }

    /// A library loaded for the probe, freed on drop.
    struct Library(HMODULE);

    impl Drop for Library {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful LoadLibraryExW and is freed once.
            let _ = unsafe { FreeLibrary(self.0) };
        }
    }

    /// Whether `mfplat.dll` loads from System32 and exports `MFStartup`. Only System32
    /// is searched, so a stray copy next to the exe can't satisfy the check.
    fn media_foundation_loads() -> bool {
        // SAFETY: plain call with a static, null-terminated name. Loading mfplat.dll
        // runs only its own initialisation, which has no requirements on the caller.
        let Ok(module) =
            (unsafe { LoadLibraryExW(w!("mfplat.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) })
        else {
            return false;
        };
        let lib = Library(module);
        // SAFETY: the module is loaded for as long as `lib` lives; the name is static and
        // null-terminated. The address is only tested, never called.
        unsafe { GetProcAddress(lib.0, s!("MFStartup")) }.is_some()
    }

    /// The registry's `EditionID`, or `None` if it can't be read.
    fn edition_id() -> Option<String> {
        let mut buf = [0u16; 128];
        let mut bytes = u32::try_from(std::mem::size_of_val(&buf)).ok()?;
        // SAFETY: `buf` is writable for `bytes` bytes, and `bytes` is a valid in/out
        // pointer; RRF_RT_REG_SZ makes the call null-terminate the string it writes.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
                w!("EditionID"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let id = String::from_utf16_lossy(&buf[..len]);
        (!id.is_empty()).then_some(id)
    }

    #[cfg(test)]
    mod tests {
        use jstream_platform::PlatformError;

        /// The real probe runs on any Windows machine without failing in another way. CI
        /// runners are Windows Server, which may lack Media Foundation (R-TEST-004), so
        /// either answer passes.
        #[test]
        fn the_probe_answers() {
            match super::check_media_foundation() {
                Ok(()) | Err(PlatformError::MissingComponent { .. }) => {}
                Err(other) => panic!("unexpected error: {other}"),
            }
        }

        /// The edition is read from this machine's registry, which always has one.
        #[test]
        fn the_edition_is_read() {
            let id = super::edition_id();
            assert!(id.is_some_and(|id| !id.is_empty()), "no EditionID");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Probe, decide, kind};
    use jstream_platform::PlatformError;

    fn missing(edition: Option<&str>) -> String {
        let probe = Probe {
            media_foundation: false,
            edition: edition.map(str::to_owned),
        };
        match decide(&probe) {
            Err(e @ PlatformError::MissingComponent { component, .. }) => {
                assert_eq!(component, "Media Foundation");
                e.to_string()
            }
            other => panic!("expected MissingComponent, got {other:?}"),
        }
    }

    #[test]
    fn present_is_ok_on_every_edition() {
        for edition in [
            None,
            Some("Professional"),
            Some("ProfessionalN"),
            Some("ServerStandard"),
        ] {
            let probe = Probe {
                media_foundation: true,
                edition: edition.map(str::to_owned),
            };
            assert_eq!(decide(&probe), Ok(()), "{edition:?}");
        }
    }

    #[test]
    fn editions_are_classified() {
        for e in [
            "CoreN",
            "ProfessionalN",
            "EnterpriseN",
            "EducationN",
            "EnterpriseSN",
            "ProfessionalKN",
            "CoreKN",
        ] {
            assert_eq!(kind(Some(e)), Kind::NEdition, "{e}");
        }
        for e in ["ServerStandard", "ServerDatacenter"] {
            assert_eq!(kind(Some(e)), Kind::Server, "{e}");
        }
        for e in [
            "Core",
            "Professional",
            "Education",
            "Enterprise",
            "IoTEnterprise",
            "",
        ] {
            assert_eq!(kind(Some(e)), Kind::Other, "{e}");
        }
        assert_eq!(kind(None), Kind::Other);
    }

    #[test]
    fn n_edition_points_at_the_media_feature_pack() {
        let msg = missing(Some("ProfessionalN"));
        assert!(
            msg.starts_with("Media Foundation is not installed: "),
            "{msg}"
        );
        assert!(
            msg.contains("N edition of Windows (ProfessionalN)"),
            "{msg}"
        );
        assert!(msg.contains("Media Feature Pack"), "{msg}");
        assert!(!msg.contains("Server"), "{msg}");
    }

    #[test]
    fn server_points_at_the_server_feature() {
        let msg = missing(Some("ServerStandard"));
        assert!(msg.contains("Windows Server (ServerStandard)"), "{msg}");
        assert!(msg.contains("Server-Media-Foundation"), "{msg}");
        assert!(!msg.contains("Media Feature Pack"), "{msg}");
    }

    #[test]
    fn unknown_edition_names_both_remedies() {
        for edition in [None, Some("Professional")] {
            let msg = missing(edition);
            assert!(msg.contains("mfplat.dll"), "{msg}");
            assert!(msg.contains("Media Feature Pack"), "{msg}");
            assert!(msg.contains("Server Media Foundation"), "{msg}");
        }
    }
}
