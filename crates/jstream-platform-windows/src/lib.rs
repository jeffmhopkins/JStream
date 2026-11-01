//! Windows platform backends: the implementations of the `jstream-platform` traits on
//! Windows. It has the QPC media clock (`QpcClock`), WASAPI audio I/O (`WasapiAudio`),
//! Windows.Graphics.Capture display and window capture (`WgcCapture`), Media Foundation
//! codecs (`MfCodecs`), webcams and capture cards (`MfVideoDevices`, with the MJPEG
//! decode hook `MfMjpegDecoder`), GPU interop (`WindowsGpu`), the Credential Manager
//! store for stream keys (`WindowsCredentials`), the virtual camera's engine side
//! (`WindowsVcam`), local media files through the Media Foundation Source Reader
//! (`MfMediaDecoders`), the shared block of an out-of-process CLAP filter (`PluginBlock`,
//! both ends) with the plugin host's MMCSS registration (`ProAudioThread`),
//! `check_media_foundation`, which reports a Windows N edition without Media Foundation,
//! and user-only files for a host's secrets (`create_private_file`,
//! `check_private_file`), and the process's CPU time (`WindowsProcessCpu`); packet video decoders follow. Layer 2, and one of the few crates allowed
//! `unsafe` (R-UNSAFE-001): every OS call is wrapped here behind safe APIs.
//!
//! The backends exist only on Windows. On other platforms the crate builds empty except
//! for its platform-independent helpers, which are tested everywhere.
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#gpu-pipeline, docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0061-source-types-and-decoding-scope-v2.md, docs/adr/0014-media-clock.md, docs/security/threat-model.md#vcam

mod audio;
mod capture;
mod clock;
mod codec;
#[cfg(windows)]
mod com;
mod credential;
mod decode;
#[cfg(windows)]
mod gpu;
// Its decision logic is tested on every platform.
#[cfg(any(windows, test))]
mod n_edition;
#[cfg(windows)]
mod plugin_block;
#[cfg(windows)]
mod plugin_hosts;
#[cfg(windows)]
mod private_file;
#[cfg(any(windows, test))]
mod process;
// Its owner and DACL rule is tested on every platform.
#[cfg(any(windows, test))]
mod security;
mod vcam;
// Its path, content and DACL decisions are tested on every platform.
#[cfg(any(windows, test))]
mod vcam_register;
mod video_device;

#[cfg(windows)]
pub use audio::WasapiAudio;
#[cfg(windows)]
pub use capture::WgcCapture;
#[cfg(windows)]
pub use clock::QpcClock;
pub use clock::timebase;
#[cfg(windows)]
pub use codec::MfCodecs;
pub use credential::CredentialTarget;
#[cfg(windows)]
pub use credential::WindowsCredentials;
#[cfg(windows)]
pub use decode::MfMediaDecoders;
#[cfg(windows)]
pub use gpu::WindowsGpu;
#[cfg(windows)]
pub use n_edition::check_media_foundation;
#[cfg(windows)]
pub use plugin_block::{PluginBlock, ProAudioThread};
#[cfg(windows)]
pub use plugin_hosts::{PLUGIN_HOST_EXE, WindowsPluginHosts};
#[cfg(windows)]
pub use private_file::{check_private_file, create_private_file};
#[cfg(windows)]
pub use process::WindowsProcessCpu;
#[cfg(windows)]
pub use vcam::WindowsVcam;
#[cfg(windows)]
pub use vcam_register::{
    Access, COPY_DACL, Outdated, Trustee, Unregistered, VCAM_DLL, VcamRegistration, is_elevated,
    program_files, register_vcam, run_elevated, unregister_vcam, vcam_copy_dir, vcam_copy_path,
    vcam_registration,
};
#[cfg(windows)]
pub use video_device::{MfMjpegDecoder, MfVideoDevices};
