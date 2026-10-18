//! Screen and window capture through ScreenCaptureKit (ADR-0063 §3).
//!
//! [`ScreenCaptureKitCapture`] lists displays and windows from `SCShareableContent`, with
//! stable IDs (`display:<CGDirectDisplayID>` and `window:<CGWindowID>`), so agents and
//! the UI pick them the same way as on Windows. Each capture is an `SCStream` that
//! delivers BGRA frames in sRGB on a serial dispatch queue; each frame is converted to
//! NV12 on the CPU with the converter every platform shares and put in the capture
//! mailbox.
//!
//! Before listing or capturing, the backend checks the Screen Recording permission. When
//! it isn't granted, it asks macOS once to prompt for it and reports an error naming the
//! System Settings pane; it never retries on its own.
//!
//! `policy` holds the parts that make no system call, so they are tested on every
//! platform; `sck` is the backend itself.

#[cfg(any(target_os = "macos", test))]
mod policy;
#[cfg(target_os = "macos")]
mod sck;

#[cfg(target_os = "macos")]
pub use sck::ScreenCaptureKitCapture;
