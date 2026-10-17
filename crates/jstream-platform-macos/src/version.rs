//! The running macOS release and its ADR-0063 support tier (§2).
//!
//! The floor is macOS 14.0. Some backends need a later point release: Core Audio process
//! taps, which desktop and application audio use, need 14.2 ([`Feature`]). Apple Silicon
//! is the supported and tested architecture, and Intel is best effort. Which releases
//! Apple still patches changes every year, so it is a docs fact
//! (`macos-security-updated`), not code: the code knows only the floor and the
//! architecture.

use std::fmt;

use jstream_platform::PlatformError;

/// A macOS release, as `NSProcessInfo.operatingSystemVersion` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MacosVersion {
    /// The major version, such as 14 for Sonoma or 26 for Tahoe.
    pub major: u32,
    /// The minor version.
    pub minor: u32,
    /// The patch version.
    pub patch: u32,
}

impl MacosVersion {
    /// ADR-0063's floor: macOS 14.0 (Sonoma).
    pub const FLOOR: Self = Self::new(14, 0, 0);

    /// A version from its parts.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses a version string such as `14.2.1`, `15.0` or `26`. Missing parts are 0.
    /// `None` for anything else, including an empty string or a fourth part.
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.trim().split('.');
        let mut next = |required: bool| match parts.next() {
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok(),
            None if !required => Some(0),
            _ => None,
        };
        let version = Self::new(next(true)?, next(false)?, next(false)?);
        parts.next().is_none().then_some(version)
    }

    /// The running macOS release.
    #[cfg(target_os = "macos")]
    pub fn current() -> Self {
        let v = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
        let part = |n: isize| u32::try_from(n).unwrap_or(0);
        Self::new(
            part(v.majorVersion),
            part(v.minorVersion),
            part(v.patchVersion),
        )
    }
}

impl fmt::Display for MacosVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A Mac's processor architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arch {
    /// Apple Silicon (`aarch64-apple-darwin`).
    AppleSilicon,
    /// Intel (`x86_64-apple-darwin`).
    Intel,
}

impl Arch {
    /// The architecture this build is for. A build for Intel running under Rosetta on
    /// Apple Silicon reports `Intel`: that is what its backends see.
    pub const fn of_build() -> Self {
        if cfg!(target_arch = "x86_64") {
            Self::Intel
        } else {
            Self::AppleSilicon
        }
    }
}

/// How ADR-0063 §2 supports a Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SupportTier {
    /// Supported and tested: Apple Silicon at or above the floor. (The tested releases
    /// are those Apple still patches, a docs fact.)
    Supported,
    /// Best effort: Intel at or above the floor. Builds and bug fixes, no testing.
    BestEffort,
    /// Below the floor.
    NotSupported,
}

/// The tier of `version` on `arch`.
pub fn support_tier(version: MacosVersion, arch: Arch) -> SupportTier {
    match (version >= MacosVersion::FLOOR, arch) {
        (false, _) => SupportTier::NotSupported,
        (true, Arch::AppleSilicon) => SupportTier::Supported,
        (true, Arch::Intel) => SupportTier::BestEffort,
    }
}

/// Checks `version` against the floor.
///
/// # Errors
/// [`PlatformError::Unsupported`] below macOS 14.0.
pub fn check_floor(version: MacosVersion) -> Result<(), PlatformError> {
    if version >= MacosVersion::FLOOR {
        Ok(())
    } else {
        Err(PlatformError::Unsupported {
            what: format!(
                "macOS {version} (JStream needs macOS {} or later, ADR-0063)",
                MacosVersion::FLOOR
            ),
        })
    }
}

/// A capability that needs a later release than the floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Core Audio process taps, for desktop and application audio (ADR-0063 §4).
    ProcessTaps,
}

impl Feature {
    /// The first release that has it.
    pub const fn since(self) -> MacosVersion {
        match self {
            Self::ProcessTaps => MacosVersion::new(14, 2, 0),
        }
    }

    /// What it is, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ProcessTaps => "Core Audio process taps",
        }
    }

    /// Checks that `version` has it.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`], with the update to install, when `version` is
    /// older than [`since`](Feature::since).
    pub fn check(self, version: MacosVersion) -> Result<(), PlatformError> {
        if version >= self.since() {
            return Ok(());
        }
        Err(PlatformError::MissingComponent {
            component: self.name(),
            remedy: format!(
                "update macOS to {} or later (installed: {version}) in System Settings › \
                 General › Software Update.",
                self.since()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::PlatformError;

    use super::{Arch, Feature, MacosVersion, SupportTier, check_floor, support_tier};

    #[test]
    fn versions_parse() {
        assert_eq!(
            MacosVersion::parse("14.2.1"),
            Some(MacosVersion::new(14, 2, 1))
        );
        assert_eq!(
            MacosVersion::parse("15.0"),
            Some(MacosVersion::new(15, 0, 0))
        );
        assert_eq!(
            MacosVersion::parse(" 26 "),
            Some(MacosVersion::new(26, 0, 0))
        );
        for bad in ["", "14.", ".1", "14.2.1.3", "fourteen", "14.x", "-14"] {
            assert_eq!(MacosVersion::parse(bad), None, "{bad:?}");
        }
        assert_eq!(MacosVersion::new(14, 2, 1).to_string(), "14.2.1");
    }

    #[test]
    fn versions_order_numerically() {
        assert!(MacosVersion::new(14, 10, 0) > MacosVersion::new(14, 2, 0));
        assert!(MacosVersion::new(26, 0, 0) > MacosVersion::new(15, 7, 9));
    }

    #[test]
    fn tiers_follow_adr_0063() {
        let sonoma = MacosVersion::new(14, 0, 0);
        let ventura = MacosVersion::new(13, 7, 8);
        assert_eq!(
            support_tier(sonoma, Arch::AppleSilicon),
            SupportTier::Supported
        );
        assert_eq!(support_tier(sonoma, Arch::Intel), SupportTier::BestEffort);
        assert_eq!(
            support_tier(ventura, Arch::AppleSilicon),
            SupportTier::NotSupported
        );
        assert_eq!(
            support_tier(ventura, Arch::Intel),
            SupportTier::NotSupported
        );
        assert!(check_floor(sonoma).is_ok());
        assert!(matches!(
            check_floor(ventura),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    #[test]
    fn process_taps_need_14_2() {
        assert!(
            Feature::ProcessTaps
                .check(MacosVersion::new(14, 2, 0))
                .is_ok()
        );
        assert!(
            Feature::ProcessTaps
                .check(MacosVersion::new(26, 0, 0))
                .is_ok()
        );
        match Feature::ProcessTaps.check(MacosVersion::new(14, 1, 2)) {
            Err(PlatformError::MissingComponent { component, remedy }) => {
                assert_eq!(component, "Core Audio process taps");
                assert!(remedy.contains("14.2.0"), "{remedy}");
                assert!(remedy.contains("14.1.2"), "{remedy}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_running_release_is_read() {
        let v = MacosVersion::current();
        assert!(v.major >= 10, "{v}");
    }
}
