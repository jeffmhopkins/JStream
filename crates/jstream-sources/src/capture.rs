//! The `display`, `window` and `webcam` sources: what the platform captures.
//!
//! Their frames don't come from [`Source::frame`]. The platform's capture backend (for
//! `webcam`, its video-device backend) delivers them, on its own thread, to the engine's
//! render thread, as CPU frames or as textures shared with the compositor. A capture source only says what to capture
//! ([`Source::capture`]); the engine starts and reads the capture.
//!
//! A `webcam` source has audio too (ADR-0061: a video capture device comes with its
//! paired audio stream): the input the platform pairs with its device, a webcam's
//! microphone or a capture card's HDMI audio. The engine records it as it records an
//! audio input ([`webcam_device`] says which device's pair).

use crate::describe::{kind, param};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, VideoFrame};
use jstream_protocol::{CaptureTargetKind, KindDescriptor, ParamType, Settings};
use serde_json::json;
use std::sync::Arc;

/// The longest `target` setting, in characters. A Windows target ID is under 300.
const MAX_TARGET: u32 = 1024;

/// The `webcam` kind's ID.
const WEBCAM: &str = "webcam";

/// The video device a source of `kind_id` with these settings captures, whose paired
/// audio input is the source's sound: `Some(Some(id))` for the device with that ID,
/// `Some(None)` for the first device the platform lists, and `None` when `kind_id` is
/// not `webcam`. Settings are complete, as the graph stores them; a missing or mistyped
/// `device` reads as its default, the first device.
pub fn webcam_device(kind_id: &str, settings: &Settings) -> Option<Option<String>> {
    if kind_id != WEBCAM {
        return None;
    }
    let device = settings
        .get("device")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    Some((!device.is_empty()).then(|| device.to_owned()))
}

/// What a capture source shows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CaptureSpec {
    /// A whole display: the one whose capture-backend target ID is given, or the
    /// primary display when `None`.
    Display(Option<String>),
    /// One top-level window: the one whose capture-backend target ID is given, or
    /// nothing when `None`.
    Window(Option<String>),
    /// A video capture device, such as a webcam or a capture card: the one whose
    /// video-device ID is given, or the first one listed when `None`.
    Device(Option<String>),
}

/// Which of the capture kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Display,
    Window,
    Device,
}

/// The `display`, `window` or `webcam` kind.
pub(crate) struct CaptureKind {
    descriptor: KindDescriptor,
    flavor: Flavor,
}

impl CaptureKind {
    /// The `display` kind. An empty `target` captures the primary display, which is
    /// what a `display` source showed before it had settings.
    pub(crate) fn display() -> Self {
        Self::new("display", "Display capture", Flavor::Display)
    }

    /// The `window` kind. An empty `target` captures nothing.
    pub(crate) fn window() -> Self {
        Self::new("window", "Window capture", Flavor::Window)
    }

    /// The `webcam` kind: a webcam or capture card. An empty `device` captures the
    /// first device the platform lists.
    pub(crate) fn webcam() -> Self {
        Self::new(WEBCAM, "Webcam", Flavor::Device)
    }

    fn new(id: &str, name: &str, flavor: Flavor) -> Self {
        let (param_id, label) = match flavor {
            Flavor::Display => ("target", "Display"),
            Flavor::Window => ("target", "Window"),
            Flavor::Device => ("device", "Device"),
        };
        // A display or window names a `capture.targets` entry, so a UI can list them.
        let ty = match flavor {
            Flavor::Display => ParamType::CaptureTarget {
                kind: CaptureTargetKind::Display,
                max_len: MAX_TARGET,
            },
            Flavor::Window => ParamType::CaptureTarget {
                kind: CaptureTargetKind::Window,
                max_len: MAX_TARGET,
            },
            Flavor::Device => ParamType::String {
                multiline: false,
                max_len: MAX_TARGET,
            },
        };
        let mut target = param(param_id, label, ty, json!(""));
        target.description = Some(match flavor {
            Flavor::Window => "The ID of the window to capture, as `capture.targets` lists \
                               it. Empty captures nothing."
                .into(),
            Flavor::Display => "The ID of the display to capture, as `capture.targets` lists \
                                it. Empty captures the primary display."
                .into(),
            Flavor::Device => "The ID of the webcam or capture card, as the platform lists \
                               it. Empty captures the first device listed."
                .into(),
        });
        let mut params = vec![target];
        // A webcam has no pointer to show; a display or window capture may leave it out.
        if flavor != Flavor::Device {
            let mut show_cursor = param("show_cursor", "Show cursor", ParamType::Bool, json!(true));
            show_cursor.description = Some("Whether the pointer shows in the capture.".into());
            params.push(show_cursor);
        }
        Self {
            descriptor: KindDescriptor {
                params,
                // A webcam's paired audio is its sound; a display or window has none.
                ..kind(id, name, true, flavor == Flavor::Device)
            },
            flavor,
        }
    }

    /// What the settings say to capture.
    fn spec(&self, settings: &Settings) -> Result<CaptureSpec, SourceError> {
        let param = &self.descriptor.params[0].id;
        let value = setting(settings, param)?;
        let target = value.as_str().ok_or_else(|| SourceError::Setting {
            param: param.clone(),
            reason: "must be a string".into(),
        })?;
        let target = (!target.is_empty()).then(|| target.to_owned());
        Ok(match self.flavor {
            Flavor::Display => CaptureSpec::Display(target),
            Flavor::Window => CaptureSpec::Window(target),
            Flavor::Device => CaptureSpec::Device(target),
        })
    }
}

impl SourceKind for CaptureKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.descriptor
    }

    fn create(&self, settings: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        let show_cursor = match self.flavor {
            Flavor::Device => true,
            Flavor::Display | Flavor::Window => setting(settings, "show_cursor")?
                .as_bool()
                .ok_or_else(|| SourceError::Setting {
                    param: "show_cursor".into(),
                    reason: "must be a boolean".into(),
                })?,
        };
        Ok(Box::new(CaptureSource {
            kind_id: self.descriptor.kind_id.clone(),
            descriptor: self.descriptor.clone(),
            settings: settings.clone(),
            spec: self.spec(settings)?,
            show_cursor,
        }))
    }
}

/// A running `display`, `window` or `webcam` source.
struct CaptureSource {
    kind_id: String,
    descriptor: KindDescriptor,
    settings: Settings,
    spec: CaptureSpec,
    show_cursor: bool,
}

impl Source for CaptureSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Err(SourceError::Captured {
            kind_id: self.kind_id.clone(),
        })
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        self.settings = settings.clone();
        Ok(())
    }

    fn capture(&self) -> Option<&CaptureSpec> {
        Some(&self.spec)
    }

    fn show_cursor(&self) -> bool {
        self.show_cursor
    }
}
