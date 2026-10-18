//! The `display`, `window` and `webcam` sources: what the platform captures.
//!
//! Their frames don't come from [`Source::frame`]. The platform's capture backend (for
//! `webcam`, its video-device backend) delivers them, on its own thread, to the engine's
//! render thread, as CPU frames or as textures shared with the compositor. A capture source only says what to capture
//! ([`Source::capture`]); the engine starts and reads the capture.

use crate::describe::{kind, param};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, VideoFrame};
use jstream_protocol::{KindDescriptor, ParamType, Settings};
use serde_json::json;
use std::sync::Arc;

/// The longest `target` setting, in characters. A Windows target ID is under 300.
const MAX_TARGET: u32 = 1024;

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
        Self::new("webcam", "Webcam", Flavor::Device)
    }

    fn new(id: &str, name: &str, flavor: Flavor) -> Self {
        let (param_id, label) = match flavor {
            Flavor::Display => ("target", "Display"),
            Flavor::Window => ("target", "Window"),
            Flavor::Device => ("device", "Device"),
        };
        let mut target = param(
            param_id,
            label,
            ParamType::String {
                multiline: false,
                max_len: MAX_TARGET,
            },
            json!(""),
        );
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
        Self {
            descriptor: KindDescriptor {
                params: vec![target],
                ..kind(id, name, true, false)
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
        Ok(Box::new(CaptureSource {
            kind_id: self.descriptor.kind_id.clone(),
            descriptor: self.descriptor.clone(),
            settings: settings.clone(),
            spec: self.spec(settings)?,
        }))
    }
}

/// A running `display`, `window` or `webcam` source.
struct CaptureSource {
    kind_id: String,
    descriptor: KindDescriptor,
    settings: Settings,
    spec: CaptureSpec,
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
}
