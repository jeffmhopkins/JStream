//! Webcams through AVFoundation (ADR-0063 §10).
//!
//! [`AvFoundationVideoDevices`] lists built-in, Continuity and external (USB) cameras from
//! an `AVCaptureDeviceDiscoverySession`, under their `uniqueID`s, with their formats and
//! the audio input that belongs to each when one can be paired by model. Each open camera
//! is an `AVCaptureSession` whose `AVCaptureVideoDataOutput` asks for NV12, which
//! AVFoundation converts or decodes (MJPEG) to, so every format is delivered raw and no
//! `CaptureDecoder` is needed. Frames are copied out into the same single-slot mailbox
//! display capture uses.
//!
//! Before a camera opens, the backend checks the Camera permission, as it checks the
//! Microphone permission before an audio input (`crate::audio`'s `permission`).
//!
//! `policy` holds the parts that make no system call, so they are tested on every
//! platform; `avf` is the backend itself.

#[cfg(target_os = "macos")]
mod avf;
#[cfg(any(target_os = "macos", test))]
mod policy;

#[cfg(target_os = "macos")]
pub use avf::AvFoundationVideoDevices;
