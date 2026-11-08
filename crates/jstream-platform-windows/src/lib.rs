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
//! `check_private_file`), and the process's CPU time (`WindowsProcessCpu`) and its threads' (`WindowsThreadCpu`), and the MMCSS tasks of the real-time threads (`WindowsThreadPriority`); packet video decoders follow. Layer 2, and one of the few crates allowed
//! `unsafe` (R-UNSAFE-001): every OS call is wrapped here behind safe APIs.
//!
//! The backends exist only on Windows. On other platforms the crate builds empty except
//! for its platform-independent helpers, which are tested everywhere.
//!
//! # Tests
//!
//! The file-dialog filter (which extensions a row keeps, All files beside them, which
//! paths may start the dialog, and the suggested file name), the clock conversion, the
//! stream helpers (format choice, sample copies, packet flags, positions across a
//! device move), the capture helpers (target IDs; the BGRA and RGBA to NV12 converter
//! and the mailbox are `jstream-platform`'s, tested there), the AAC helpers
//! (configuration check, PCM conversion, packet timing and padding), the video encoder
//! helpers (configuration check, NV12 packing and RGBA/BGRA conversion, `pts` mapping),
//! the first-frame fallback with fake encoders (`codec::fallback`: a refused first
//! frame reopened in software, a later refusal failing, `Busy` keeping it armed, a
//! failed software open, and when it applies), the GPU capture ring's slot choice
//! (reuse, drop when full, release on resize), the credential helpers (target names of
//! both kinds, blob limits, zeroing), the virtual camera's format and frame checks and
//! the Media Foundation check's decision (edition kinds and the advice for each) and
//! the video-device helpers (subtypes and frame rates to formats, repeats left out,
//! colour fallbacks, strides, short buffers reported against the buffer lock, audio
//! pairing by container; the NV12 and I420 copies and YUY2 to NV12 are `jstream-
//! platform`'s, tested there) and the media-file helpers (RGB32 rows to RGBA and BGRA,
//! top-down and bottom-up, cropping to an aperture, short buffers, packed sizes and
//! rates, 100 ns times, the verbatim path prefix, the gapless cut of audio priming and
//! padding, and reading an MP3's LAME tag) run on every platform. The QPC clock test,
//! the start-race test (a destroyed message-only window maps to `NotFound`), the
//! credential limits check, the credential round trip (a uniquely named credential
//! written, replaced, read and deleted in the real store, a non-UTF-8 one refused on
//! read, deleted again on failure), the same through `StreamKeyStore`, a refresh token
//! through `OAuthTokenStore` beside a stream key of the same ID, and the Media
//! Foundation probe and edition read run on Windows. Tests against real devices, the OS
//! encoder, the GPU and a display are platform tests: behind the `hw-tests` feature and
//! `#[ignore]`d, because CI machines may have no audio devices, no Media Foundation, no
//! GPU and no display (R-TEST-004). The GPU test renders into an encode target, stages
//! it and checks that the shared fence passes the staged value. The GPU encode tests
//! open the H.264, HEVC and AV1 encoders on the context, write a ring of three 720p
//! encode targets with wgpu for 60 frames, checking that the encoder has let go of each
//! target before it is written again, encode them, refuse a CPU frame, and check the
//! packets as the CPU encode tests do: each codec's framing, and the parameter sets (or
//! AV1 sequence header) in the first keyframe. HEVC and AV1 run only on a GPU that
//! encodes them (AV1 needs an NVIDIA RTX 40, AMD RX 7000, Intel Arc or newer). The
//! capture tests list the displays, capture the primary display for a second, checking
//! the frames' format and `pts`, start a capture of a destroyed window, which must be
//! `NotFound`, and capture three times in a row in one process. The audio tests list
//! the devices (one default each way), record the default input for a second on the
//! media clock, play a quiet tone and record its loopback, check that `input_format`
//! matches the opened stream for the default input and the default output's loopback,
//! and find this process in the process list, record its process loopback and get
//! `NotFound` for a process that isn't running. The GPU capture tests read back twenty
//! staged frames (each opaque, so a read before the copy finished would show), refuse
//! another device's queue, outlive the context, and hold every frame until the ring
//! drops new ones, then release them. They need screen activity, so move the mouse
//! while they run. The virtual-camera test starts `WindowsVcam`, opens the camera's own
//! media source through a source reader (video processing on, RGB32 out), and publishes
//! grey until Frame Server serves it back; it also checks that a second producer is
//! refused and that the block is stopped after `stop`. The video-device tests list
//! devices, formats and paired audio, capture an uncompressed format and the largest
//! MJPEG format for two seconds each (checking size, format and `pts`), open a device
//! twice (shared or `Busy`), and refuse an unknown device or format; an unplug test
//! runs only when `JSTREAM_UNPLUG_TEST` is set. The H.264 tests encode two seconds in
//! software from NV12 and from RGBA (the engine's input), and in hardware from RGBA;
//! the HEVC and AV1 tests encode two seconds in hardware from RGBA. The media-file
//! tests play the video file `JSTREAM_TEST_MEDIA` names (over two seconds long): thirty
//! frames in order, opaque and of the stream's size, a seek to the middle landing at or
//! before it, a seek past the end ending the stream, a second of its audio as
//! contiguous `f32` blocks and a seek of it, no audio when none is asked for, and the
//! errors for a missing file, a file that isn't media and an unsupported pixel format.
//! Run them on a Windows machine with a microphone, speakers, a display, a webcam and a
//! DX12 GPU with hardware H.264, HEVC and AV1 encoders: `cargo test -p jstream-
//! platform-windows --features hw-tests -- --ignored`. The virtual-camera test also
//! needs the `jstream-vcam-windows` DLL registered (its Hardware check).
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
// The extension filter is tested on every platform; the dialog itself is Windows.
#[cfg(any(windows, test))]
mod dialog;
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
pub use dialog::{Pick, PickKind, pick_path};
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
pub use process::{WindowsProcessCpu, WindowsThreadCpu, WindowsThreadPriority};
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
