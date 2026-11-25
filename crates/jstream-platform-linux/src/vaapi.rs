//! VA-API, loaded at run time (ADR-0060 §6, §8).
//!
//! [`probe_encoders`] opens each DRM render node through libva and lists the H.264,
//! HEVC and AV1 encode entry points its driver offers, and `VaapiEncoder` is an encode
//! session on one of them (`session`). The parameters it submits are built in `h264`,
//! `hevc` and `av1`, the structs libva reads are declared in `ffi`, and CPU frames are
//! written into VA images by `upload`; those five are platform-independent and tested
//! everywhere.
//!
//! Decoding goes the other way: `VaapiH264Decoder` (`decoder`) decodes a media file's
//! H.264 on a render node whose driver offers the stream's profile, from the pictures
//! `media::h264`'s planner lays out, and reads each frame back. The picture, scaling
//! list and slice parameters it submits are built in `h264_dec`, platform-independent
//! and tested everywhere too. `VaapiVp9Decoder` (`vp9_decoder`) does the same for VP9
//! from `media::vp9`'s frame headers, its parameters built in `vp9_dec`, and
//! `VaapiVp8Decoder` (`vp8_decoder`) for VP8 from `media::vp8`'s, its parameters built
//! in `vp8_dec`, and `VaapiAv1Decoder` (`av1_decoder`) for AV1 from `media::av1`'s, its
//! parameters built in `av1_dec`; all four share `decoder::Core`, the context, surfaces
//! and read-back.

// Used by the Linux session; built elsewhere for its tests, which don't use the
// parameter buffers the session submits.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux session; built elsewhere for its tests"
    )
)]
mod av1;
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux decoder; built elsewhere for its tests"
    )
)]
mod av1_dec;
#[cfg(target_os = "linux")]
mod av1_decoder;
#[cfg(target_os = "linux")]
mod decoder;
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
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux decoder; built elsewhere for its tests"
    )
)]
mod h264_dec;
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
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux decoder; built elsewhere for its tests"
    )
)]
mod vp8_dec;
#[cfg(target_os = "linux")]
mod vp8_decoder;
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "used by the Linux decoder; built elsewhere for its tests"
    )
)]
mod vp9_dec;
#[cfg(target_os = "linux")]
mod vp9_decoder;

use jstream_platform::media::Codec;

#[cfg(target_os = "linux")]
pub(crate) use av1_decoder::VaapiAv1Decoder;
#[cfg(target_os = "linux")]
pub(crate) use decoder::VaapiH264Decoder;
#[cfg(target_os = "linux")]
pub(crate) use session::VaapiEncoder;
#[cfg(target_os = "linux")]
pub(crate) use vp8_decoder::VaapiVp8Decoder;
#[cfg(target_os = "linux")]
pub(crate) use vp9_decoder::VaapiVp9Decoder;

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
    /// `VAProfileAV1Profile0`: AV1 Main, 8-bit 4:2:0 here.
    Av1Main,
}

impl VaProfile {
    /// libva's `VAProfile` value.
    pub fn va(self) -> i32 {
        match self {
            Self::H264(profile) => profile as i32,
            Self::HevcMain => 17,
            Self::Av1Main => 32,
        }
    }

    /// The codec it encodes.
    pub fn codec(self) -> Codec {
        match self {
            Self::H264(_) => Codec::H264,
            Self::HevcMain => Codec::Hevc,
            Self::Av1Main => Codec::Av1,
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

/// One H.264, HEVC or AV1 encoder a VA-API driver offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaEncoder {
    /// The DRM render node, such as `/dev/dri/renderD128`.
    pub node: String,
    /// The driver's vendor string.
    pub vendor: String,
    /// The profile it encodes: the best H.264 profile, HEVC Main or AV1 Main.
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
    /// node its best H.264 profile, then HEVC Main, then AV1 Main, each when the driver
    /// encodes it. Empty when there is no render node or no driver encodes any of them
    /// (Fedora's stock Mesa, NVIDIA's driver).
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
            let others = [VaProfile::HevcMain, VaProfile::Av1Main]
                .into_iter()
                .filter_map(|p| VaEntrypoint::choose(&display.entrypoints(p.va())).map(|e| (p, e)));
            for (profile, entrypoint) in h264.into_iter().chain(others) {
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
        assert_eq!(VaProfile::Av1Main.va(), 32);
        assert_eq!(VaProfile::Av1Main.codec(), Codec::Av1);
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
