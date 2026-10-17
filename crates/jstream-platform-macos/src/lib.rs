//! macOS platform backends: the implementations of the `jstream-platform` traits on macOS,
//! as ADR-0063 lays them out, built in parts. It has the `mach_absolute_time`
//! media clock (`MachClock`), the running release and its support tier (`MacosVersion`,
//! `support_tier`), four-character codes and `OSStatus` errors, the VideoToolbox and
//! AudioToolbox settings for an encoder configuration, the VideoToolbox H.264 and HEVC
//! encoder on CPU frames (`VideoToolboxCodecs`, with Annex B output from
//! `annexb_access_unit`), the AudioToolbox AAC-LC encoder, whose raw access units are
//! framed and timed by `AacFramer`, and display and window capture through
//! ScreenCaptureKit (`ScreenCaptureKitCapture`) on CPU frames, behind the Screen Recording
//! permission, and the Keychain store for stream keys and OAuth tokens (`MacosKeychain`).
//! Audio (`CoreAudioIo`) and decoding report `Unsupported` until their
//! follow-ups land. Layer 2, and one of the crates allowed `unsafe` (R-UNSAFE-001): every
//! system call is wrapped here behind safe APIs.
//!
//! Apple's frameworks are called through the objc2 crates and Mach time through `mach2`
//! (ADR-0063 §12), so the crate type-checks on any host for an Apple target. The backends
//! exist only on macOS. On other platforms the crate builds its platform-independent
//! helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-macos/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0063-macos-platform-support.md, docs/adr/0061-source-types-and-decoding-scope-v2.md, docs/adr/0014-media-clock.md

mod aac;
mod annexb;
mod audio;
#[cfg(target_os = "macos")]
mod audiotoolbox;
mod capture;
mod clock;
mod codec;
mod fourcc;
mod keychain;
mod version;
#[cfg(target_os = "macos")]
mod videotoolbox;

pub use aac::{AAC_FRAME, AacFramer, aac_lc_config, asc_from_magic_cookie, nearest_bit_rate};
pub use annexb::{annexb_access_unit, cm_time_parts, copy_rows, media_time_from_cm};
#[cfg(target_os = "macos")]
pub use audio::CoreAudioIo;
pub use audio::{check_input, feature_for_input};
#[cfg(target_os = "macos")]
pub use capture::ScreenCaptureKitCapture;
#[cfg(target_os = "macos")]
pub use clock::MachClock;
pub use clock::{hns_from_mach, timebase};
#[cfg(target_os = "macos")]
pub use codec::VideoToolboxCodecs;
pub use codec::{
    AUDIO_AAC, AacSettings, CM_H264, CM_HEVC, VtBitRate, VtEncoderSettings, aac_settings,
    vt_encoder_settings,
};
pub use fourcc::{
    CV_420V, CV_BGRA, CV_RGBA, CV_X420, CV_Y420, cv_pixel_format, fourcc, fourcc_str, os_status,
    pixel_format_from_cv,
};
pub use keychain::KeychainItem;
#[cfg(target_os = "macos")]
pub use keychain::MacosKeychain;
pub use version::{Arch, Feature, MacosVersion, SupportTier, check_floor, support_tier};
