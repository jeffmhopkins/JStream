//! VA-API, loaded at run time (ADR-0060 §6, §8).
//!
//! [`probe_encoders`] opens each DRM render node through libva and lists the H.264 and
//! HEVC encode entry points its driver offers, and `VaapiEncoder` is an encode session
//! on one of them (`session`). The parameters it submits are built in `h264` and
//! `hevc`, the structs libva reads are declared in `ffi`, and CPU frames are written
//! into VA images by `upload`; those four are platform-independent and tested
//! everywhere.

#[cfg(target_os = "linux")]
mod display;
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux session; built elsewhere for its tests"
    )
)]
mod ffi;
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux session; built elsewhere for its tests"
    )
)]
mod h264;
// Used by the Linux session; built elsewhere for its tests, which use all of it.
#[cfg(any(target_os = "linux", test))]
mod hevc;
#[cfg(target_os = "linux")]
mod session;
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux session; built elsewhere for its tests"
    )
)]
mod upload;

use jstream_platform::media::Codec;

#[cfg(target_os = "linux")]
pub(crate) use session::VaapiEncoder;

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

/// What a VA-API encoder encodes: a libva `VAProfile`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaProfile {
    /// An H.264 profile.
    H264(H264Profile),
    /// `VAProfileHEVCMain`: HEVC Main, 8-bit 4:2:0.
    HevcMain,
}

impl VaProfile {
    /// libva's `VAProfile` value.
    pub fn va(self) -> i32 {
        match self {
            Self::H264(profile) => profile as i32,
            Self::HevcMain => 17,
        }
    }

    /// The codec it encodes.
    pub fn codec(self) -> Codec {
        match self {
            Self::H264(_) => Codec::H264,
            Self::HevcMain => Codec::Hevc,
        }
    }
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

/// One H.264 or HEVC encoder a VA-API driver offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaEncoder {
    /// The DRM render node, such as `/dev/dri/renderD128`.
    pub node: String,
    /// The driver's vendor string.
    pub vendor: String,
    /// The profile it encodes: the best H.264 profile, or HEVC Main.
    pub profile: VaProfile,
    /// The entry point it encodes through.
    pub entrypoint: VaEntrypoint,
}

#[cfg(target_os = "linux")]
pub use backend::probe_encoders;

#[cfg(target_os = "linux")]
mod backend {
    use jstream_platform::PlatformError;

    use super::display::{Display, Va};
    use super::{H264Profile, VaEncoder, VaEntrypoint, VaProfile};

    /// The encoders every DRM render node's VA-API driver offers, in node order: on each
    /// node its best H.264 profile, then HEVC Main, each when the driver encodes it.
    /// Empty when there is no render node or no driver encodes either (Fedora's stock
    /// Mesa, NVIDIA's driver).
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
            // A node that can't be opened, or has no driver, has no encoder.
            let Ok(display) = Display::open(va.clone(), &node) else {
                continue;
            };
            let h264 = H264Profile::ALL
                .into_iter()
                .map(VaProfile::H264)
                .find_map(|profile| {
                    VaEntrypoint::choose(&display.entrypoints(profile.va())).map(|e| (profile, e))
                });
            let hevc = VaEntrypoint::choose(&display.entrypoints(VaProfile::HevcMain.va()))
                .map(|e| (VaProfile::HevcMain, e));
            for (profile, entrypoint) in h264.into_iter().chain(hevc) {
                found.push(VaEncoder {
                    vendor: display.vendor(),
                    node: node.clone(),
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
    use jstream_platform::media::Codec;

    use super::{H264Profile, VaEntrypoint, VaProfile};

    #[test]
    fn values_match_libva() {
        assert_eq!(H264Profile::Main as i32, 6);
        assert_eq!(H264Profile::High as i32, 7);
        assert_eq!(H264Profile::ConstrainedBaseline as i32, 13);
        assert_eq!(VaProfile::H264(H264Profile::High).va(), 7);
        assert_eq!(VaProfile::HevcMain.va(), 17);
        assert_eq!(VaProfile::H264(H264Profile::Main).codec(), Codec::H264);
        assert_eq!(VaProfile::HevcMain.codec(), Codec::Hevc);
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
